use std::fmt::Display;

use serde::Deserialize;

use super::ForgeError;
use super::auth::load_credential_for;
use super::types::{
    Branch, CheckRun, CreateIssue, CreatePullRequest, CreateRepository, Issue, IssueState, Listing,
    MergeMethod, Provider, PullRequest, PullRequestChecks, PullRequestComment, PullRequestState,
    RepoRef, Repository, ThreadEntry, ThreadKind, User, normalize_check_status, rollup_from,
    sort_thread, total_from_last_page,
};

fn api_error(error: impl Display) -> ForgeError {
    ForgeError::api(Provider::Forgejo, error)
}

fn response_error(error: impl Display) -> ForgeError {
    ForgeError::Response(error.to_string())
}

use super::types::PAGE_SIZE;

/// The Forgejo list state of a pull request list: merged requests list
/// under `closed`, like on GitHub.
fn state_param(state: PullRequestState) -> &'static str {
    match state {
        PullRequestState::Open => "open",
        PullRequestState::Closed | PullRequestState::Merged => "closed",
        PullRequestState::All => "all",
    }
}

/// The Forgejo shape of a pull request. `index` is the user-facing number
/// its routes carry (`number` is an internal id); labels and reviewers
/// default because list entries may omit them.
#[derive(Deserialize)]
struct ApiPullRequest {
    number: u64,
    title: String,
    state: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    mergeable: Option<bool>,
    #[serde(default)]
    merged: bool,
    user: Option<ApiAccount>,
    body: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    #[serde(default, deserialize_with = "null_as_empty")]
    labels: Vec<ApiLabel>,
    #[serde(default, deserialize_with = "null_as_empty")]
    requested_reviewers: Vec<ApiAccount>,
    head: ApiRef,
    base: ApiRef,
}

fn null_as_empty<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<Vec<T>>::deserialize(deserializer).map(Option::unwrap_or_default)
}

#[derive(Deserialize)]
struct ApiAccount {
    login: String,
}

#[derive(Deserialize)]
struct ApiLabel {
    name: String,
}

#[derive(Deserialize)]
struct ApiRef {
    #[serde(rename = "ref")]
    branch: String,
    sha: Option<String>,
}

impl From<ApiPullRequest> for PullRequest {
    fn from(request: ApiPullRequest) -> Self {
        Self {
            number: request.number,
            title: request.title,
            state: request.state,
            html_url: request.html_url,
            head_branch: request.head.branch,
            base_branch: request.base.branch,
            draft: request.draft,
            author: request.user.map(|user| user.login),
            body: request.body,
            created_at: request.created_at,
            updated_at: request.updated_at,
            head_sha: request.head.sha,
            mergeable: request.mergeable,
            merged: request.merged,
            labels: request.labels.into_iter().map(|label| label.name).collect(),
            reviewers: request
                .requested_reviewers
                .into_iter()
                .map(|user| user.login)
                .collect(),
        }
    }
}

#[derive(Deserialize)]
struct ApiRepository {
    name: String,
    full_name: String,
    html_url: String,
    clone_url: Option<String>,
    ssh_url: Option<String>,
    private: bool,
    default_branch: Option<String>,
    description: Option<String>,
}

/// A Forgejo client: plain REST over `{host}/api/v1` with
/// `Authorization: token <token>`, like the GitLab client.
pub struct ForgejoClient {
    http: reqwest::Client,
    token: String,
    base_url: String,
}

impl ForgejoClient {
    pub fn from_token(token: String, host: Option<&str>) -> Result<Self, ForgeError> {
        let host = host
            .unwrap_or(Provider::Forgejo.default_host())
            .trim_end_matches('/');
        let base_url = format!("{host}/api/v1");
        reqwest::Url::parse(&base_url).map_err(api_error)?;
        Ok(Self {
            http: reqwest::Client::new(),
            token,
            base_url,
        })
    }

    pub fn from_environment() -> Result<Self, ForgeError> {
        let host = std::env::var("FORGEJO_HOST")
            .unwrap_or_else(|_| Provider::Forgejo.default_host().to_owned());
        let account = std::env::var("RGIT_FORGE_ACCOUNT").unwrap_or_else(|_| "default".to_owned());
        Self::from_environment_for(&host, &account)
    }

    pub fn from_environment_for(host: &str, account: &str) -> Result<Self, ForgeError> {
        let token = match Provider::Forgejo.env_token() {
            Some(token) => token,
            None => load_credential_for("forgejo", host, account)?
                .map(|value| value.access_token)
                .ok_or(ForgeError::AuthenticationRequired)?,
        };
        Self::from_token(token, Some(host))
    }

    fn authorization(&self) -> String {
        format!("token {}", self.token)
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ForgeError> {
        request
            .header(reqwest::header::AUTHORIZATION, self.authorization())
            .send()
            .await
            .map_err(api_error)?
            .error_for_status()
            .map_err(api_error)
    }

    /// The user the token belongs to.
    pub async fn current_user(&self) -> Result<User, ForgeError> {
        #[derive(Deserialize)]
        struct ApiUser {
            login: String,
            full_name: Option<String>,
            html_url: Option<String>,
            avatar_url: Option<String>,
        }
        let user: ApiUser = self
            .send(self.http.get(format!("{}/user", self.base_url)))
            .await?
            .json()
            .await
            .map_err(response_error)?;
        Ok(User {
            login: user.login,
            name: user.full_name.filter(|name| !name.is_empty()),
            html_url: user.html_url.unwrap_or_default(),
            avatar_url: user.avatar_url.unwrap_or_default(),
        })
    }

    fn repository_path(&self, repo: &RepoRef) -> String {
        format!(
            "{}/repos/{}/{}",
            self.base_url,
            urlencoding::encode(&repo.owner),
            urlencoding::encode(&repo.name)
        )
    }

    fn repository_from(repository: ApiRepository) -> Repository {
        Repository {
            name: repository.name,
            full_name: repository.full_name,
            html_url: repository.html_url,
            clone_url: repository.clone_url,
            ssh_url: repository.ssh_url,
            private: repository.private,
            default_branch: repository.default_branch,
            description: repository.description,
        }
    }

    pub async fn repository(&self, repo: &RepoRef) -> Result<Repository, ForgeError> {
        let repository: ApiRepository = self
            .send(self.http.get(self.repository_path(repo)))
            .await?
            .json()
            .await
            .map_err(response_error)?;
        Ok(Self::repository_from(repository))
    }

    pub async fn create_repository(
        &self,
        request: &CreateRepository,
        organization: Option<&str>,
    ) -> Result<Repository, ForgeError> {
        let url = match organization {
            Some(org) => format!("{}/orgs/{}/repos", self.base_url, urlencoding::encode(org)),
            None => format!("{}/user/repos", self.base_url),
        };
        let payload = serde_json::json!({
            "name": request.name,
            "description": request.description,
            "private": request.private,
            "auto_init": request.auto_init
        });
        let repository: ApiRepository = self
            .send(self.http.post(url).json(&payload))
            .await?
            .json()
            .await
            .map_err(response_error)?;
        Ok(Self::repository_from(repository))
    }

    pub async fn delete_repository(&self, repo: &RepoRef) -> Result<(), ForgeError> {
        self.send(self.http.delete(self.repository_path(repo)))
            .await
            .map(|_| ())
    }

    /// One page of a list endpoint, with Forgejo's `page` and `limit`
    /// query and the `x-total-count` header.
    async fn list_page<T: serde::de::DeserializeOwned>(
        &self,
        route: &str,
        page: u32,
    ) -> Result<Listing<T>, ForgeError> {
        let page = page.max(1);
        let sep = if route.contains('?') { '&' } else { '?' };
        let url = format!(
            "{base}{route}{sep}page={page}&limit={PAGE_SIZE}",
            base = self.base_url
        );
        let response = self.send(self.http.get(url)).await?;
        let reported: Option<u64> = response
            .headers()
            .get("x-total-count")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse().ok());
        let items: Vec<T> = response.json().await.map_err(response_error)?;
        let more = match reported {
            Some(total) => u64::from(page) * u64::from(PAGE_SIZE) < total,
            None => items.len() == PAGE_SIZE as usize,
        };
        let total = reported.or_else(|| {
            if more {
                None
            } else {
                total_from_last_page(page, items.len())
            }
        });
        Ok(Listing {
            items,
            page,
            more,
            total,
        })
    }

    /// One page of branches.
    pub async fn branches(&self, repo: &RepoRef, page: u32) -> Result<Listing<Branch>, ForgeError> {
        #[derive(Deserialize)]
        struct ApiBranch {
            name: String,
            protected: bool,
            commit: ApiCommit,
        }
        #[derive(Deserialize)]
        struct ApiCommit {
            id: String,
        }
        let route = format!(
            "/repos/{}/{}/branches",
            urlencoding::encode(&repo.owner),
            urlencoding::encode(&repo.name)
        );
        let listing: Listing<ApiBranch> = self.list_page(&route, page).await?;
        Ok(listing.map(|branch| Branch {
            name: branch.name,
            sha: branch.commit.id,
            protected: branch.protected,
        }))
    }

    pub async fn delete_branch(&self, repo: &RepoRef, branch: &str) -> Result<(), ForgeError> {
        self.send(self.http.delete(format!(
            "{}/branches/{}",
            self.repository_path(repo),
            urlencoding::encode(branch)
        )))
        .await
        .map(|_| ())
    }

    /// One page of pull requests, by `state`; `head` and `author` filter
    /// without matching anything when unset. Forgejo's pull list has no
    /// server-side head or author filter, so both are applied here, and a
    /// merged filter keeps the merged subset; each drops the page total.
    pub async fn pull_requests(
        &self,
        repo: &RepoRef,
        page: u32,
        state: PullRequestState,
        head: Option<&str>,
        author: Option<&str>,
    ) -> Result<Listing<PullRequest>, ForgeError> {
        let route = format!(
            "/repos/{}/{}/pulls?state={}",
            urlencoding::encode(&repo.owner),
            urlencoding::encode(&repo.name),
            state_param(state)
        );
        let listing: Listing<ApiPullRequest> = self.list_page(&route, page).await?;
        let only_merged = state == PullRequestState::Merged;
        let filtered = head.is_some() || author.is_some() || only_merged;
        let mut items = Vec::with_capacity(listing.items.len());
        for request in listing.items {
            if only_merged && !request.merged {
                continue;
            }
            let mapped: PullRequest = request.into();
            if head.is_some_and(|head| mapped.head_branch != head) {
                continue;
            }
            if author.is_some_and(|author| mapped.author.as_deref() != Some(author)) {
                continue;
            }
            items.push(mapped);
        }
        let total = if filtered && (listing.page != 1 || listing.more) {
            None
        } else {
            listing.total
        };
        Ok(Listing {
            items,
            page: listing.page,
            more: listing.more,
            total,
        })
    }

    /// One pull request by its index.
    pub async fn pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequest, ForgeError> {
        let request: ApiPullRequest = self
            .send(
                self.http
                    .get(format!("{}/pulls/{number}", self.repository_path(repo))),
            )
            .await?
            .json()
            .await
            .map_err(response_error)?;
        Ok(request.into())
    }

    /// Open a pull request from `request`.
    pub async fn create_pull_request(
        &self,
        repo: &RepoRef,
        request: &CreatePullRequest,
    ) -> Result<PullRequest, ForgeError> {
        let payload = serde_json::json!({
            "title": request.title,
            "head": request.head,
            "base": request.base,
            "body": request.body
        });
        let created: ApiPullRequest = self
            .send(
                self.http
                    .post(format!("{}/pulls", self.repository_path(repo)))
                    .json(&payload),
            )
            .await?
            .json()
            .await
            .map_err(response_error)?;
        Ok(created.into())
    }

    fn edit_pull_request_payload(
        title: Option<&str>,
        body: Option<&str>,
        base: Option<&str>,
        state: Option<&'static str>,
    ) -> serde_json::Value {
        let mut payload = serde_json::Map::new();
        if let Some(title) = title {
            payload.insert("title".to_owned(), serde_json::json!(title));
        }
        if let Some(body) = body {
            payload.insert("body".to_owned(), serde_json::json!(body));
        }
        if let Some(base) = base {
            payload.insert("base".to_owned(), serde_json::json!(base));
        }
        if let Some(state) = state {
            payload.insert("state".to_owned(), serde_json::json!(state));
        }
        serde_json::Value::Object(payload)
    }

    async fn patch_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
        payload: serde_json::Value,
    ) -> Result<PullRequest, ForgeError> {
        let updated: ApiPullRequest = self
            .send(
                self.http
                    .patch(format!("{}/pulls/{number}", self.repository_path(repo)))
                    .json(&payload),
            )
            .await?
            .json()
            .await
            .map_err(response_error)?;
        Ok(updated.into())
    }

    /// Move pull request `number` onto `base`.
    pub async fn retarget_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
        base: &str,
    ) -> Result<PullRequest, ForgeError> {
        self.patch_pull_request(
            repo,
            number,
            Self::edit_pull_request_payload(None, None, Some(base), None),
        )
        .await
    }

    /// Close pull request `number`.
    pub async fn close_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequest, ForgeError> {
        self.patch_pull_request(
            repo,
            number,
            Self::edit_pull_request_payload(None, None, None, Some("closed")),
        )
        .await
    }

    /// Reopen pull request `number`.
    pub async fn reopen_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequest, ForgeError> {
        self.patch_pull_request(
            repo,
            number,
            Self::edit_pull_request_payload(None, None, None, Some("open")),
        )
        .await
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
        self.patch_pull_request(
            repo,
            number,
            Self::edit_pull_request_payload(title, body, base, None),
        )
        .await
    }

    /// Merge pull request `number` with `method`, deleting its head branch
    /// when `delete_branch` is set, then report the merged pull request.
    pub async fn merge_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<PullRequest, ForgeError> {
        let style = match method {
            MergeMethod::Merge => "merge",
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
        };
        self.send(
            self.http
                .post(format!(
                    "{}/pulls/{number}/merge",
                    self.repository_path(repo)
                ))
                .json(&serde_json::json!({ "Do": style })),
        )
        .await?;
        let merged = self.pull_request(repo, number).await?;
        if delete_branch {
            self.delete_branch(repo, &merged.head_branch).await?;
        }
        Ok(merged)
    }

    /// Comment `body` on pull request `number`; pull requests share their
    /// numbers with issues, so this goes through the issue comments.
    pub async fn comment_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
        body: &str,
    ) -> Result<PullRequestComment, ForgeError> {
        #[derive(Deserialize)]
        struct ApiComment {
            id: u64,
            body: String,
            user: Option<ApiAccount>,
            html_url: Option<String>,
            created_at: Option<String>,
        }
        let comment: ApiComment = self
            .send(
                self.http
                    .post(format!(
                        "{}/issues/{number}/comments",
                        self.repository_path(repo)
                    ))
                    .json(&serde_json::json!({ "body": body })),
            )
            .await?
            .json()
            .await
            .map_err(response_error)?;
        Ok(PullRequestComment {
            id: comment.id,
            body: comment.body,
            author: comment.user.map(|user| user.login),
            url: comment.html_url,
            created_at: comment.created_at,
        })
    }

    /// The checks of pull request `number`'s head commit: its combined
    /// commit statuses, one entry per status.
    pub async fn pull_request_checks(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequestChecks, ForgeError> {
        #[derive(Deserialize)]
        struct ApiCombinedStatus {
            #[serde(default, deserialize_with = "null_as_empty")]
            statuses: Vec<ApiCommitStatus>,
        }
        #[derive(Deserialize)]
        struct ApiCommitStatus {
            status: String,
            context: String,
            target_url: Option<String>,
        }
        let pr = self.pull_request(repo, number).await?;
        let sha = pr.head_sha.ok_or_else(|| {
            ForgeError::Response(format!(
                "pull request {number} of {repo} has no head commit"
            ))
        })?;
        let combined: ApiCombinedStatus = self
            .send(self.http.get(format!(
                "{}/commits/{}/status",
                self.repository_path(repo),
                urlencoding::encode(&sha)
            )))
            .await?
            .json()
            .await
            .map_err(response_error)?;
        let mut checks = Vec::with_capacity(combined.statuses.len());
        let mut rollups = Vec::with_capacity(combined.statuses.len());
        for status in combined.statuses {
            let normalized = normalize_check_status(&status.status);
            rollups.push(normalized.to_owned());
            checks.push(CheckRun {
                name: status.context,
                status: normalized.to_owned(),
                url: status.target_url,
            });
        }
        Ok(PullRequestChecks {
            rollup: rollup_from(rollups),
            checks,
        })
    }

    /// Every item of a list endpoint, following pages until one reports
    /// no more.
    async fn all_pages<T: serde::de::DeserializeOwned>(
        &self,
        route: &str,
    ) -> Result<Vec<T>, ForgeError> {
        let mut items = Vec::new();
        let mut page = 1;
        loop {
            let listing: Listing<T> = self.list_page(route, page).await?;
            items.extend(listing.items);
            if !listing.more {
                break;
            }
            page += 1;
        }
        Ok(items)
    }
}

/// The issue shape of Forgejo's API. `pull_request` is present on pull
/// requests listed as issues and absent on real ones.
#[derive(Deserialize)]
struct ApiIssue {
    number: u64,
    title: String,
    state: String,
    user: Option<ApiAccount>,
    body: Option<String>,
    #[serde(default, deserialize_with = "null_as_empty")]
    labels: Vec<ApiLabel>,
    #[serde(default, deserialize_with = "null_as_empty")]
    assignees: Vec<ApiAccount>,
    html_url: String,
    created_at: Option<String>,
    updated_at: Option<String>,
    pull_request: Option<serde_json::Value>,
}

impl ApiIssue {
    fn is_pull_request(&self) -> bool {
        self.pull_request.is_some()
    }
}

impl From<ApiIssue> for Issue {
    fn from(issue: ApiIssue) -> Self {
        Self {
            number: issue.number,
            title: issue.title,
            state: issue.state,
            author: issue.user.map(|user| user.login),
            body: issue.body,
            labels: issue.labels.into_iter().map(|label| label.name).collect(),
            assignees: issue.assignees.into_iter().map(|user| user.login).collect(),
            url: issue.html_url,
            created_at: issue.created_at,
            updated_at: issue.updated_at,
        }
    }
}

impl ForgejoClient {
    /// One page of issues, by `state`; `label` filters server-side while
    /// `author` and `assignee` filter here, and entries carrying a
    /// `pull_request` field are dropped before mapping. Each client-side
    /// filter drops the page total.
    pub async fn issues(
        &self,
        repo: &RepoRef,
        page: u32,
        state: IssueState,
        author: Option<&str>,
        label: Option<&str>,
        assignee: Option<&str>,
    ) -> Result<Listing<Issue>, ForgeError> {
        let state = match state {
            IssueState::Open => "open",
            IssueState::Closed => "closed",
            IssueState::All => "all",
        };
        let mut route = format!(
            "/repos/{}/{}/issues?state={state}&type=issues",
            urlencoding::encode(&repo.owner),
            urlencoding::encode(&repo.name)
        );
        if let Some(label) = label {
            route.push_str(&format!("&labels={}", urlencoding::encode(label)));
        }
        let listing: Listing<ApiIssue> = self.list_page(&route, page).await?;
        let filtered = author.is_some() || assignee.is_some();
        let mut items = Vec::with_capacity(listing.items.len());
        let mut dropped = 0;
        for issue in listing.items {
            if issue.is_pull_request() {
                dropped += 1;
                continue;
            }
            let mapped: Issue = issue.into();
            if author.is_some_and(|author| mapped.author.as_deref() != Some(author)) {
                continue;
            }
            if assignee
                .is_some_and(|assignee| !mapped.assignees.iter().any(|name| name == assignee))
            {
                continue;
            }
            items.push(mapped);
        }
        let total = if (filtered || dropped > 0) && (listing.more || listing.page != 1) {
            None
        } else if dropped > 0 {
            Some(items.len() as u64)
        } else {
            listing.total
        };
        Ok(Listing {
            items,
            page: listing.page,
            more: listing.more,
            total,
        })
    }

    /// One issue by its number; a pull request number is refused, since
    /// pull requests have their own calls.
    pub async fn issue(&self, repo: &RepoRef, number: u64) -> Result<Issue, ForgeError> {
        let issue: ApiIssue = self
            .send(
                self.http
                    .get(format!("{}/issues/{number}", self.repository_path(repo))),
            )
            .await?
            .json()
            .await
            .map_err(response_error)?;
        if issue.is_pull_request() {
            return Err(ForgeError::Response(format!(
                "{repo} #{number} is a pull request, not an issue"
            )));
        }
        Ok(issue.into())
    }

    /// The label ids of `names`, looked up in one call; a name that does
    /// not resolve is an error, so a typo cannot silently drop a label.
    async fn label_ids(&self, repo: &RepoRef, names: &[String]) -> Result<Vec<u64>, ForgeError> {
        #[derive(Deserialize)]
        struct ApiLabelId {
            id: u64,
            name: String,
        }
        let route = format!(
            "/repos/{}/{}/labels",
            urlencoding::encode(&repo.owner),
            urlencoding::encode(&repo.name)
        );
        let labels: Vec<ApiLabelId> = self
            .send(
                self.http
                    .get(format!("{base}{route}?limit=100", base = self.base_url)),
            )
            .await?
            .json()
            .await
            .map_err(response_error)?;
        let mut ids = Vec::with_capacity(names.len());
        for name in names {
            let id = labels
                .iter()
                .find(|label| &label.name == name)
                .map(|label| label.id)
                .ok_or_else(|| ForgeError::Response(format!("no Forgejo label named {name}")))?;
            ids.push(id);
        }
        Ok(ids)
    }

    /// Open an issue from `request`, resolving label names to ids.
    pub async fn create_issue(
        &self,
        repo: &RepoRef,
        request: &CreateIssue,
    ) -> Result<Issue, ForgeError> {
        let mut payload = serde_json::Map::from_iter([
            ("title".to_owned(), serde_json::json!(request.title)),
            ("assignees".to_owned(), serde_json::json!(request.assignees)),
        ]);
        if let Some(body) = &request.body {
            payload.insert("body".to_owned(), serde_json::json!(body));
        }
        if !request.labels.is_empty() {
            let ids = self.label_ids(repo, &request.labels).await?;
            payload.insert("labels".to_owned(), serde_json::json!(ids));
        }
        let issue: ApiIssue = self
            .send(
                self.http
                    .post(format!("{}/issues", self.repository_path(repo)))
                    .json(&payload),
            )
            .await?
            .json()
            .await
            .map_err(response_error)?;
        Ok(issue.into())
    }

    /// Close issue `number`.
    pub async fn close_issue(&self, repo: &RepoRef, number: u64) -> Result<Issue, ForgeError> {
        self.set_issue_state(repo, number, "closed").await
    }

    /// Reopen issue `number`.
    pub async fn reopen_issue(&self, repo: &RepoRef, number: u64) -> Result<Issue, ForgeError> {
        self.set_issue_state(repo, number, "open").await
    }

    async fn set_issue_state(
        &self,
        repo: &RepoRef,
        number: u64,
        state: &'static str,
    ) -> Result<Issue, ForgeError> {
        self.issue(repo, number).await?;
        let issue: ApiIssue = self
            .send(
                self.http
                    .patch(format!("{}/issues/{number}", self.repository_path(repo)))
                    .json(&serde_json::json!({ "state": state })),
            )
            .await?
            .json()
            .await
            .map_err(response_error)?;
        Ok(issue.into())
    }

    /// Comment `body` on issue `number`.
    pub async fn comment_issue(
        &self,
        repo: &RepoRef,
        number: u64,
        body: &str,
    ) -> Result<PullRequestComment, ForgeError> {
        self.issue(repo, number).await?;
        self.comment_pull_request(repo, number, body).await
    }

    /// Change the title, body, labels and assignees issue `number`
    /// carries; each `None` (or empty list) leaves that field alone.
    /// Labels go through the replace-labels endpoint, since editing an
    /// issue cannot set them.
    pub async fn edit_issue(
        &self,
        repo: &RepoRef,
        number: u64,
        title: Option<&str>,
        body: Option<&str>,
        labels: Option<&[String]>,
        assignees: Option<&[String]>,
    ) -> Result<Issue, ForgeError> {
        self.issue(repo, number).await?;
        let mut payload = serde_json::Map::new();
        if let Some(title) = title {
            payload.insert("title".to_owned(), serde_json::json!(title));
        }
        if let Some(body) = body {
            payload.insert("body".to_owned(), serde_json::json!(body));
        }
        if let Some(assignees) = assignees.filter(|names| !names.is_empty()) {
            payload.insert("assignees".to_owned(), serde_json::json!(assignees));
        }
        let issue: ApiIssue = self
            .send(
                self.http
                    .patch(format!("{}/issues/{number}", self.repository_path(repo)))
                    .json(&payload),
            )
            .await?
            .json()
            .await
            .map_err(response_error)?;
        let Some(labels) = labels.filter(|names| !names.is_empty()) else {
            return Ok(issue.into());
        };
        let ids = self.label_ids(repo, labels).await?;
        self.send(
            self.http
                .put(format!(
                    "{}/issues/{number}/labels",
                    self.repository_path(repo)
                ))
                .json(&serde_json::json!({ "labels": ids })),
        )
        .await?;
        self.issue(repo, number).await
    }

    /// The whole conversation on issue `number`: its comments, oldest
    /// first.
    pub async fn issue_thread(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<Vec<ThreadEntry>, ForgeError> {
        self.issue(repo, number).await?;
        #[derive(Deserialize)]
        struct ApiComment {
            id: u64,
            #[serde(default)]
            body: String,
            user: Option<ApiAccount>,
            html_url: Option<String>,
            created_at: Option<String>,
        }
        let route = format!(
            "/repos/{}/{}/issues/{number}/comments",
            urlencoding::encode(&repo.owner),
            urlencoding::encode(&repo.name)
        );
        let mut entries: Vec<ThreadEntry> = self
            .all_pages::<ApiComment>(&route)
            .await?
            .into_iter()
            .map(|comment| ThreadEntry {
                kind: ThreadKind::Comment,
                id: Some(comment.id),
                author: comment.user.map(|user| user.login),
                body: comment.body,
                created_at: comment.created_at,
                url: comment.html_url,
                path: None,
                line: None,
                state: None,
                in_reply_to: None,
                resolved: None,
            })
            .collect();
        sort_thread(&mut entries);
        Ok(entries)
    }
}
