use octocrab::Octocrab;
use serde::{Deserialize, Serialize};

use super::ForgeError;
use super::auth::{load_credential_for, refresh_if_needed};
use super::types::{
    Branch, CheckRun, CreateIssue, CreatePullRequest, CreateRepository, Issue, IssueState, Listing,
    MergeMethod, PAGE_SIZE, Provider, PullRequest, PullRequestChecks, PullRequestComment,
    PullRequestReview, PullRequestState, RepoRef, Repository, ReviewAction, ThreadEntry,
    ThreadKind, User, normalize_check_status, page_query, rollup_from, sort_thread,
    total_from_last_page,
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
            merged: request.merged || request.merged_at.is_some(),
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
        let node_id = value
            .get("node_id")
            .and_then(|id| id.as_str())
            .ok_or_else(|| {
                ForgeError::Response(format!("pull request {number} of {repo} has no node id"))
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

    /// Comment `body` on pull request `number`.
    pub async fn comment_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
        body: &str,
    ) -> Result<PullRequestComment, ForgeError> {
        #[derive(Serialize)]
        struct NewComment<'a> {
            body: &'a str,
        }
        #[derive(Deserialize)]
        struct ApiComment {
            id: u64,
            body: String,
            user: Option<ApiAccount>,
            html_url: String,
            created_at: Option<String>,
        }
        let comment: ApiComment = self
            .api
            .post(
                format!(
                    "/repos/{}/{}/issues/{number}/comments",
                    repo.owner, repo.name
                ),
                Some(&NewComment { body }),
            )
            .await
            .map_err(api_error)?;
        Ok(PullRequestComment {
            id: comment.id,
            body: comment.body,
            author: comment.user.map(|user| user.login),
            url: Some(comment.html_url),
            created_at: comment.created_at,
        })
    }

    /// Review pull request `number`: approve it, request changes, or leave a
    /// comment review, with `body` as the review text.
    pub async fn review_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
        action: ReviewAction,
        body: Option<&str>,
    ) -> Result<PullRequestReview, ForgeError> {
        #[derive(Serialize)]
        struct NewReview<'a> {
            event: &'static str,
            #[serde(skip_serializing_if = "Option::is_none")]
            body: Option<&'a str>,
        }
        #[derive(Deserialize)]
        struct ApiReview {
            id: u64,
            state: String,
            body: Option<String>,
            user: Option<ApiAccount>,
            html_url: Option<String>,
        }
        let review: ApiReview = self
            .api
            .post(
                format!("/repos/{}/{}/pulls/{number}/reviews", repo.owner, repo.name),
                Some(&NewReview {
                    event: action.github_event(),
                    body,
                }),
            )
            .await
            .map_err(api_error)?;
        Ok(PullRequestReview {
            id: Some(review.id),
            state: review.state.to_lowercase(),
            body: review.body,
            author: review.user.map(|user| user.login),
            url: review.html_url,
        })
    }

    /// The whole conversation on pull request `number`: plain comments,
    /// inline code comments (with file and line) and reviews, oldest first.
    /// GitHub files one empty COMMENTED review per inline batch, so those
    /// are skipped; the inline entries already carry the text. Pending
    /// reviews are other users' drafts and never visible here.
    pub async fn pull_request_thread(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<Vec<ThreadEntry>, ForgeError> {
        #[derive(Deserialize)]
        struct ApiComment {
            id: u64,
            #[serde(default)]
            body: String,
            user: Option<ApiAccount>,
            html_url: Option<String>,
            created_at: Option<String>,
        }
        #[derive(Deserialize)]
        struct ApiInline {
            id: u64,
            #[serde(default)]
            body: String,
            user: Option<ApiAccount>,
            html_url: Option<String>,
            created_at: Option<String>,
            path: Option<String>,
            line: Option<u64>,
            original_line: Option<u64>,
            in_reply_to_id: Option<u64>,
        }
        #[derive(Deserialize)]
        struct ApiReview {
            id: u64,
            state: String,
            body: Option<String>,
            user: Option<ApiAccount>,
            html_url: Option<String>,
            submitted_at: Option<String>,
        }
        let mut entries = Vec::new();
        for comment in self
            .all_pages::<ApiComment>(&format!(
                "/repos/{}/{}/issues/{number}/comments",
                repo.owner, repo.name
            ))
            .await?
        {
            entries.push(ThreadEntry {
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
            });
        }
        for inline in self
            .all_pages::<ApiInline>(&format!(
                "/repos/{}/{}/pulls/{number}/comments",
                repo.owner, repo.name
            ))
            .await?
        {
            entries.push(ThreadEntry {
                kind: ThreadKind::Inline,
                id: Some(inline.id),
                author: inline.user.map(|user| user.login),
                body: inline.body,
                created_at: inline.created_at,
                url: inline.html_url,
                path: inline.path,
                line: inline.line.or(inline.original_line),
                state: None,
                in_reply_to: inline.in_reply_to_id,
                resolved: None,
            });
        }
        for review in self
            .all_pages::<ApiReview>(&format!(
                "/repos/{}/{}/pulls/{number}/reviews",
                repo.owner, repo.name
            ))
            .await?
        {
            if review.state.eq_ignore_ascii_case("pending") {
                continue;
            }
            let body = review.body.unwrap_or_default();
            if review.state.eq_ignore_ascii_case("commented") && body.trim().is_empty() {
                continue;
            }
            entries.push(ThreadEntry {
                kind: ThreadKind::Review,
                id: Some(review.id),
                author: review.user.map(|user| user.login),
                body,
                created_at: review.submitted_at,
                url: review.html_url,
                path: None,
                line: None,
                state: Some(review.state.to_lowercase()),
                in_reply_to: None,
                resolved: None,
            });
        }
        sort_thread(&mut entries);
        Ok(entries)
    }

    /// The comments on issue `number`, oldest first; a pull request number is
    /// refused, since pull requests have their own calls.
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
        let mut entries: Vec<ThreadEntry> = self
            .all_pages::<ApiComment>(&format!(
                "/repos/{}/{}/issues/{number}/comments",
                repo.owner, repo.name
            ))
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

    /// Every item of a list endpoint, following pages until a short one.
    async fn all_pages<T: serde::de::DeserializeOwned>(
        &self,
        route: &str,
    ) -> Result<Vec<T>, ForgeError> {
        let mut items = Vec::new();
        let mut page = 1;
        loop {
            let batch: Vec<T> = self
                .api
                .get(page_query(route, page), None::<&()>)
                .await
                .map_err(api_error)?;
            let short = (batch.len() as u32) < PAGE_SIZE;
            items.extend(batch);
            if short {
                break;
            }
            page += 1;
        }
        Ok(items)
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

    /// One page of issues. GitHub lists pull requests as issues, so entries
    /// carrying a `pull_request` field are dropped before mapping.
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
        let mut route = format!("/repos/{}/{}/issues?state={state}", repo.owner, repo.name);
        if let Some(author) = author {
            route.push_str(&format!("&creator={}", urlencoding::encode(author)));
        }
        if let Some(label) = label {
            route.push_str(&format!("&labels={}", urlencoding::encode(label)));
        }
        if let Some(assignee) = assignee {
            route.push_str(&format!("&assignee={}", urlencoding::encode(assignee)));
        }
        let listing: Listing<ApiIssue> = self.list_page(&route, page).await?;
        let dropped = listing
            .items
            .iter()
            .filter(|issue| issue.is_pull_request())
            .count();
        let mut listing: Listing<Issue> =
            listing.map_filter(|issue| (!issue.is_pull_request()).then(|| issue.into()));
        if dropped > 0 && (listing.more || listing.page != 1) {
            listing.total = None;
        } else if dropped > 0 {
            listing.total = Some(listing.items.len() as u64);
        }
        Ok(listing)
    }

    /// One issue by its number; a pull request number is refused, since pull
    /// requests have their own calls.
    pub async fn issue(&self, repo: &RepoRef, number: u64) -> Result<Issue, ForgeError> {
        let issue: ApiIssue = self
            .api
            .get(
                format!("/repos/{}/{}/issues/{number}", repo.owner, repo.name),
                None::<&()>,
            )
            .await
            .map_err(api_error)?;
        if issue.is_pull_request() {
            return Err(ForgeError::Response(format!(
                "{repo} #{number} is a pull request, not an issue"
            )));
        }
        Ok(issue.into())
    }

    /// Open an issue from `request`.
    pub async fn create_issue(
        &self,
        repo: &RepoRef,
        request: &CreateIssue,
    ) -> Result<Issue, ForgeError> {
        #[derive(Serialize)]
        struct NewIssue<'a> {
            title: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            body: &'a Option<String>,
            labels: &'a [String],
            assignees: &'a [String],
        }
        let issue: ApiIssue = self
            .api
            .post(
                format!("/repos/{}/{}/issues", repo.owner, repo.name),
                Some(&NewIssue {
                    title: &request.title,
                    body: &request.body,
                    labels: &request.labels,
                    assignees: &request.assignees,
                }),
            )
            .await
            .map_err(api_error)?;
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
        #[derive(Serialize)]
        struct Update {
            state: &'static str,
        }
        self.issue(repo, number).await?;
        let issue: ApiIssue = self
            .api
            .patch(
                format!("/repos/{}/{}/issues/{number}", repo.owner, repo.name),
                Some(&Update { state }),
            )
            .await
            .map_err(api_error)?;
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
        self.issue(repo, number).await?;
        let mut patch = serde_json::Map::new();
        if let Some(title) = title {
            patch.insert("title".to_owned(), serde_json::json!(title));
        }
        if let Some(body) = body {
            patch.insert("body".to_owned(), serde_json::json!(body));
        }
        if let Some(labels) = labels {
            patch.insert("labels".to_owned(), serde_json::json!(labels));
        }
        if let Some(assignees) = assignees {
            patch.insert("assignees".to_owned(), serde_json::json!(assignees));
        }
        let issue: ApiIssue = self
            .api
            .patch(
                format!("/repos/{}/{}/issues/{number}", repo.owner, repo.name),
                Some(&serde_json::Value::Object(patch)),
            )
            .await
            .map_err(api_error)?;
        Ok(issue.into())
    }
}

/// The issue shape of GitHub's API. `pull_request` is present on pull
/// requests listed as issues and absent on real ones.
#[derive(serde::Deserialize)]
struct ApiIssue {
    number: u64,
    title: String,
    state: String,
    user: Option<ApiAccount>,
    body: Option<String>,
    #[serde(default)]
    labels: Vec<ApiLabel>,
    #[serde(default)]
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

/// Octocrab returns the raw JSON value for a POST/PATCH; map the API's shape.
fn map_pr(value: serde_json::Value) -> Result<PullRequest, ForgeError> {
    serde_json::from_value::<ApiPullRequest>(value)
        .map(PullRequest::from)
        .map_err(|e| ForgeError::Response(e.to_string()))
}
