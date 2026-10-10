use octocrab::Octocrab;
use serde::{Deserialize, Serialize};

use super::ForgeError;
use super::auth::{load_credential_for, refresh_if_needed};
use super::types::{
    Branch, CheckRun, CreatePullRequest, CreateRepository, Listing, MergeMethod, Provider,
    PullRequest, PullRequestChecks, PullRequestState, RepoRef, Repository, User,
    normalize_check_status, page_query, rollup_from, total_from_last_page,
};

fn api_error(error: octocrab::Error) -> ForgeError {
    let message = error.to_string();
    if message == "GitHub" {
        // octocrab prints this for an error it cannot classify, so its Debug
        // form is the only thing that says what failed.
        ForgeError::api(Provider::Github, format!("{message}: {error:?}"))
    } else {
        ForgeError::api(Provider::Github, message)
    }
}

/// The `state` query value. GitHub has no merged listing: merged pull requests
/// arrive under `closed` and are filtered by `merged_at`.
fn state_param(state: PullRequestState) -> &'static str {
    match state {
        PullRequestState::Open => "open",
        PullRequestState::Closed | PullRequestState::Merged => "closed",
        PullRequestState::All => "all",
    }
}

/// The pull request shape of GitHub's API; rgit's type flattens `head` and
/// `base` into branch names.
#[derive(serde::Deserialize)]
struct ApiPullRequest {
    number: u64,
    title: String,
    state: String,
    html_url: String,
    draft: bool,
    merged_at: Option<String>,
    #[serde(default)]
    merged: bool,
    user: Option<ApiAccount>,
    body: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    mergeable: Option<bool>,
    #[serde(default)]
    labels: Vec<ApiLabel>,
    #[serde(default)]
    requested_reviewers: Vec<ApiAccount>,
    head: ApiRef,
    base: ApiRef,
}

#[derive(serde::Deserialize)]
struct ApiAccount {
    login: String,
}

#[derive(serde::Deserialize)]
struct ApiLabel {
    name: String,
}

#[derive(serde::Deserialize)]
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

pub struct GithubClient {
    api: Octocrab,
}

impl GithubClient {
    pub fn from_token(token: String) -> Result<Self, ForgeError> {
        Self::from_token_at(token, None)
    }

    /// A client whose API base is `base_url` instead of github.com's API;
    /// tests point it at a local stub. `None` is octocrab's default.
    pub fn from_token_at(token: String, base_url: Option<&str>) -> Result<Self, ForgeError> {
        let builder = Octocrab::builder().personal_token(token);
        let builder = match base_url {
            Some(url) => builder.base_uri(url).map_err(api_error)?,
            None => builder,
        };
        builder.build().map(|api| Self { api }).map_err(api_error)
    }

    pub async fn from_environment() -> Result<Self, ForgeError> {
        let account = std::env::var("RGIT_FORGE_ACCOUNT").unwrap_or_else(|_| "default".to_owned());
        Self::from_environment_for(&account, None).await
    }

    pub async fn from_environment_for(
        account: &str,
        base_url: Option<&str>,
    ) -> Result<Self, ForgeError> {
        if let Some(token) = Provider::Github.env_token() {
            return Self::from_token_at(token, base_url);
        }
        let credential = load_credential_for("github", "github.com", account)?
            .ok_or(ForgeError::AuthenticationRequired)?;
        let credential = refresh_if_needed(credential, "github", "github.com", account).await?;
        Self::from_token_at(credential.access_token, base_url)
    }

    pub async fn repository(&self, repo: &RepoRef) -> Result<Repository, ForgeError> {
        self.api
            .get(format!("/repos/{}/{}", repo.owner, repo.name), None::<&()>)
            .await
            .map_err(api_error)
    }

    pub async fn current_user(&self) -> Result<User, ForgeError> {
        self.api.get("/user", None::<&()>).await.map_err(api_error)
    }

    pub async fn create_repository(
        &self,
        request: &CreateRepository,
        organization: Option<&str>,
    ) -> Result<Repository, ForgeError> {
        let route = organization
            .map(|org| format!("/orgs/{org}/repos"))
            .unwrap_or_else(|| "/user/repos".to_owned());
        self.api.post(route, Some(request)).await.map_err(api_error)
    }

    pub async fn delete_repository(&self, repo: &RepoRef) -> Result<(), ForgeError> {
        self.api
            ._delete(format!("/repos/{}/{}", repo.owner, repo.name), None::<&()>)
            .await
            .map(|_| ())
            .map_err(api_error)
    }

    /// One page of a list endpoint. When a `last` link is present, that page
    /// is fetched too so the exact total can be reported.
    async fn list_page<T: serde::de::DeserializeOwned>(
        &self,
        route: &str,
        page: u32,
    ) -> Result<Listing<T>, ForgeError> {
        let page = page.max(1);
        let first: octocrab::Page<T> = self
            .api
            .get(page_query(route, page), None::<&()>)
            .await
            .map_err(api_error)?;
        let more = first.next.is_some();
        let last_page = first.last.as_ref().and_then(|uri| {
            uri.query()?
                .split('&')
                .find_map(|kv| kv.strip_prefix("page="))
                .and_then(|n| n.parse::<u32>().ok())
        });
        let total = match last_page {
            Some(last) if last != page => {
                let tail: Option<octocrab::Page<serde_json::Value>> =
                    self.api.get_page(&first.last).await.map_err(api_error)?;
                tail.and_then(|t| total_from_last_page(last, t.items.len()))
            }
            _ if !more => total_from_last_page(page, first.items.len()),
            _ => None,
        };
        Ok(Listing {
            items: first.items,
            page,
            more,
            total,
        })
    }

    pub async fn branches(&self, repo: &RepoRef, page: u32) -> Result<Listing<Branch>, ForgeError> {
        #[derive(Deserialize)]
        struct ApiBranch {
            name: String,
            protected: bool,
            commit: ApiCommit,
        }
        #[derive(Deserialize)]
        struct ApiCommit {
            sha: String,
        }
        let listing: Listing<ApiBranch> = self
            .list_page(
                &format!("/repos/{}/{}/branches", repo.owner, repo.name),
                page,
            )
            .await?;
        Ok(listing.map(|branch| Branch {
            name: branch.name,
            sha: branch.commit.sha,
            protected: branch.protected,
        }))
    }

    pub async fn delete_branch(&self, repo: &RepoRef, branch: &str) -> Result<(), ForgeError> {
        let branch = urlencoding::encode(branch);
        self.api
            ._delete(
                format!(
                    "/repos/{}/{}/git/refs/heads/{branch}",
                    repo.owner, repo.name
                ),
                None::<&()>,
            )
            .await
            .map(|_| ())
            .map_err(api_error)
    }

    pub async fn pull_requests(
        &self,
        repo: &RepoRef,
        page: u32,
        state: PullRequestState,
        head: Option<&str>,
        author: Option<&str>,
    ) -> Result<Listing<PullRequest>, ForgeError> {
        let mut route = format!(
            "/repos/{}/{}/pulls?state={}",
            repo.owner,
            repo.name,
            state_param(state)
        );
        if let Some(branch) = head {
            route.push_str(&format!(
                "&head={}:{}",
                repo.owner,
                urlencoding::encode(branch)
            ));
        }
        let listing: Listing<ApiPullRequest> = self.list_page(&route, page).await?;
        // GitHub lists merged pull requests only under state=closed, so the
        // merged filter keeps the merged_at subset and drops the page total;
        // the author filter is applied here because GitHub's pull list has no
        // server-side equivalent, and it drops the total for the same reason.
        let only_merged = state == PullRequestState::Merged;
        let mut items = Vec::with_capacity(listing.items.len());
        for request in listing.items {
            if only_merged && request.merged_at.is_none() {
                continue;
            }
            let mapped: PullRequest = request.into();
            if author.is_some_and(|author| mapped.author.as_deref() != Some(author)) {
                continue;
            }
            items.push(mapped);
        }
        let total = if (only_merged || author.is_some()) && (listing.page != 1 || listing.more) {
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

    pub async fn pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequest, ForgeError> {
        self.pull_request_value(repo, number).await.and_then(map_pr)
    }

    async fn pull_request_value(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<serde_json::Value, ForgeError> {
        self.api
            .get(
                format!("/repos/{}/{}/pulls/{number}", repo.owner, repo.name),
                None::<&()>,
            )
            .await
            .map_err(api_error)
    }

    /// Mark pull request `number` ready for review or draft. REST cannot flip
    /// the draft flag, so this goes through GraphQL, then reports the pull
    /// request as REST sees it.
    pub async fn set_pull_request_draft(
        &self,
        repo: &RepoRef,
        number: u64,
        draft: bool,
    ) -> Result<PullRequest, ForgeError> {
        let value = self.pull_request_value(repo, number).await?;
        let node_id = value.get("node_id").and_then(|id| id.as_str()).ok_or_else(|| {
            ForgeError::Response(format!(
                "pull request {number} of {repo} has no node id"
            ))
        })?;
        let mutation = if draft {
            "convertPullRequestToDraft"
        } else {
            "markPullRequestReadyForReview"
        };
        let _: serde_json::Value = self
            .api
            .graphql(&serde_json::json!({
                "query": format!(
                    "mutation($id: ID!) {{ {mutation}(input: {{pullRequestId: $id}}) {{ pullRequest {{ id isDraft }} }} }}"
                ),
                "variables": { "id": node_id },
            }))
            .await
            .map_err(api_error)?;
        self.pull_request(repo, number).await
    }

    /// The checks of pull request `number`'s head commit: GitHub's check runs
    /// and commit statuses combined, one entry per check.
    pub async fn pull_request_checks(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequestChecks, ForgeError> {
        let pr = self.pull_request(repo, number).await?;
        let sha = pr.head_sha.ok_or_else(|| {
            ForgeError::Response(format!(
                "pull request {number} of {repo} has no head commit"
            ))
        })?;
        #[derive(serde::Deserialize)]
        struct ApiCheckRuns {
            #[serde(default)]
            check_runs: Vec<ApiCheckRun>,
        }
        #[derive(serde::Deserialize)]
        struct ApiCheckRun {
            name: String,
            status: String,
            conclusion: Option<String>,
            html_url: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct ApiStatuses {
            #[serde(default)]
            statuses: Vec<ApiCommitStatus>,
        }
        #[derive(serde::Deserialize)]
        struct ApiCommitStatus {
            state: String,
            context: String,
            target_url: Option<String>,
        }
        let runs: ApiCheckRuns = self
            .api
            .get(
                format!(
                    "/repos/{}/{}/commits/{sha}/check-runs?per_page=100",
                    repo.owner, repo.name
                ),
                None::<&()>,
            )
            .await
            .map_err(api_error)?;
        let statuses: ApiStatuses = self
            .api
            .get(
                format!(
                    "/repos/{}/{}/commits/{sha}/status?per_page=100",
                    repo.owner, repo.name
                ),
                None::<&()>,
            )
            .await
            .map_err(api_error)?;
        let mut checks = Vec::with_capacity(runs.check_runs.len() + statuses.statuses.len());
        let mut rollups = Vec::with_capacity(checks.capacity());
        for run in runs.check_runs {
            let status = normalize_check_status(run.conclusion.as_deref().unwrap_or(&run.status));
            rollups.push(status.to_owned());
            checks.push(CheckRun {
                name: run.name,
                status: status.to_owned(),
                url: run.html_url,
            });
        }
        for status in statuses.statuses {
            let normalized = normalize_check_status(&status.state);
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

    pub async fn create_pull_request(
        &self,
        repo: &RepoRef,
        request: &CreatePullRequest,
    ) -> Result<PullRequest, ForgeError> {
        #[derive(Serialize)]
        struct ApiRequest<'a> {
            title: &'a str,
            head: &'a str,
            base: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            body: &'a Option<String>,
            draft: bool,
        }
        self.api
            .post(
                format!("/repos/{}/{}/pulls", repo.owner, repo.name),
                Some(&ApiRequest {
                    title: &request.title,
                    head: &request.head,
                    base: &request.base,
                    body: &request.body,
                    draft: request.draft,
                }),
            )
            .await
            .map_err(api_error)
            .and_then(map_pr)
    }

    pub async fn retarget_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
        base: &str,
    ) -> Result<PullRequest, ForgeError> {
        #[derive(Serialize)]
        struct Retarget<'a> {
            base: &'a str,
        }
        self.api
            .patch(
                format!("/repos/{}/{}/pulls/{number}", repo.owner, repo.name),
                Some(&Retarget { base }),
            )
            .await
            .map_err(api_error)
            .and_then(map_pr)
    }

    pub async fn close_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequest, ForgeError> {
        #[derive(Serialize)]
        struct Update {
            state: &'static str,
        }
        self.api
            .patch(
                format!("/repos/{}/{}/pulls/{number}", repo.owner, repo.name),
                Some(&Update { state: "closed" }),
            )
            .await
            .map_err(api_error)
            .and_then(map_pr)
    }

    /// Reopen pull request `number`.
    pub async fn reopen_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequest, ForgeError> {
        #[derive(Serialize)]
        struct Update {
            state: &'static str,
        }
        self.api
            .patch(
                format!("/repos/{}/{}/pulls/{number}", repo.owner, repo.name),
                Some(&Update { state: "open" }),
            )
            .await
            .map_err(api_error)
            .and_then(map_pr)
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
        let mut patch = serde_json::Map::new();
        if let Some(title) = title {
            patch.insert("title".to_owned(), serde_json::json!(title));
        }
        if let Some(body) = body {
            patch.insert("body".to_owned(), serde_json::json!(body));
        }
        if let Some(base) = base {
            patch.insert("base".to_owned(), serde_json::json!(base));
        }
        self.api
            .patch(
                format!("/repos/{}/{}/pulls/{number}", repo.owner, repo.name),
                Some(&serde_json::Value::Object(patch)),
            )
            .await
            .map_err(api_error)
            .and_then(map_pr)
    }

    /// Merge pull request `number` with `method`: GitHub merges through its
    /// merge endpoint, then reports the merged pull request.
    pub async fn merge_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<PullRequest, ForgeError> {
        #[derive(Serialize)]
        struct Merge<'a> {
            merge_method: &'a str,
        }
        #[derive(Deserialize)]
        struct MergeResult {
            merged: bool,
            message: Option<String>,
        }
        let result: MergeResult = self
            .api
            .put(
                format!("/repos/{}/{}/pulls/{number}/merge", repo.owner, repo.name),
                Some(&Merge {
                    merge_method: method.as_str(),
                }),
            )
            .await
            .map_err(api_error)?;
        if !result.merged {
            return Err(ForgeError::api(
                Provider::Github,
                result.message.unwrap_or_else(|| {
                    format!("pull request {number} of {repo} could not be merged")
                }),
            ));
        }
        let merged = self.pull_request(repo, number).await?;
        if delete_branch {
            self.delete_branch(repo, &merged.head_branch).await?;
        }
        Ok(merged)
    }
}

/// Octocrab returns the raw JSON value for a POST/PATCH; map the API's shape.
fn map_pr(value: serde_json::Value) -> Result<PullRequest, ForgeError> {
    serde_json::from_value::<ApiPullRequest>(value)
        .map(PullRequest::from)
        .map_err(|e| ForgeError::Response(e.to_string()))
}
