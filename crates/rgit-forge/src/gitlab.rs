use std::fmt::Display;

use serde::Deserialize;

use super::ForgeError;
use super::auth::load_credential_for;
use super::types::{
    Branch, CheckRun, CreatePullRequest, CreateRepository, Listing, MergeMethod, Provider,
    PullRequest, PullRequestChecks, PullRequestComment, PullRequestState, RepoRef, Repository,
    User, normalize_check_status, page_query, rollup_from, total_from_last_page,
};

const DEFAULT_HOST: &str = "https://gitlab.com";

fn state_param(state: PullRequestState) -> &'static str {
    match state {
        PullRequestState::Open => "opened",
        PullRequestState::Closed => "closed",
        PullRequestState::Merged => "merged",
        PullRequestState::All => "all",
    }
}

fn api_error(error: impl Display) -> ForgeError {
    ForgeError::api(Provider::Gitlab, error)
}

fn response_error(error: impl Display) -> ForgeError {
    ForgeError::Response(error.to_string())
}

/// The merge request shape of GitLab's API; rgit's type flattens the branches
/// and maps `merge_status` onto `mergeable`.
#[derive(Deserialize)]
struct ApiMergeRequest {
    iid: u64,
    title: String,
    state: String,
    web_url: String,
    #[serde(default)]
    draft: bool,
    author: Option<ApiAccount>,
    description: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    sha: Option<String>,
    merge_status: Option<String>,
    labels: Option<serde_json::Value>,
    reviewers: Option<Vec<ApiAccount>>,
    source_branch: String,
    target_branch: String,
}

#[derive(Deserialize)]
struct ApiAccount {
    username: String,
}

impl ApiMergeRequest {
    fn into_pull_request(self) -> PullRequest {
        let merged = self.state == "merged";
        PullRequest {
            number: self.iid,
            title: self.title,
            state: self.state,
            html_url: self.web_url,
            head_branch: self.source_branch,
            base_branch: self.target_branch,
            draft: self.draft,
            author: self.author.map(|user| user.username),
            body: self.description,
            created_at: self.created_at,
            updated_at: self.updated_at,
            head_sha: self.sha,
            mergeable: self
                .merge_status
                .map(|status| status.starts_with("can_be_")),
            merged,
            labels: labels_from_value(self.labels),
            reviewers: self
                .reviewers
                .unwrap_or_default()
                .into_iter()
                .map(|user| user.username)
                .collect(),
        }
    }
}

/// `labels` is a string array (`["bug"]`) or, with labels details on, a
/// `[{"name": "bug"}]` object array.
fn labels_from_value(value: Option<serde_json::Value>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    match value {
        serde_json::Value::Array(items) => items
            .into_iter()
            .filter_map(|item| match item {
                serde_json::Value::String(name) => Some(name),
                serde_json::Value::Object(fields) => fields
                    .get("name")
                    .and_then(|name| name.as_str())
                    .map(str::to_owned),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub struct GitlabClient {
    http: reqwest::Client,
    token: String,
    base_url: String,
}

impl GitlabClient {
    pub fn from_token(token: String, host: Option<&str>) -> Result<Self, ForgeError> {
        let host = host.unwrap_or(DEFAULT_HOST).trim_end_matches('/');
        let base_url = format!("{host}/api/v4");
        reqwest::Url::parse(&base_url).map_err(api_error)?;
        Ok(Self {
            http: reqwest::Client::new(),
            token,
            base_url,
        })
    }

    pub fn from_environment() -> Result<Self, ForgeError> {
        let host = std::env::var("GITLAB_HOST").unwrap_or_else(|_| DEFAULT_HOST.to_owned());
        let account = std::env::var("RGIT_FORGE_ACCOUNT").unwrap_or_else(|_| "default".to_owned());
        Self::from_environment_for(&host, &account)
    }

    pub fn from_environment_for(host: &str, account: &str) -> Result<Self, ForgeError> {
        let token = match Provider::Gitlab.env_token() {
            Some(token) => token,
            None => load_credential_for("gitlab", host, account)?
                .map(|value| value.access_token)
                .ok_or(ForgeError::AuthenticationRequired)?,
        };
        Self::from_token(token, Some(host))
    }

    pub async fn current_user(&self) -> Result<User, ForgeError> {
        #[derive(Deserialize)]
        struct ApiUser {
            username: String,
            name: Option<String>,
            web_url: String,
            avatar_url: Option<String>,
        }
        let user: ApiUser = self
            .http
            .get(format!("{}/user", self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(api_error)?
            .error_for_status()
            .map_err(api_error)?
            .json()
            .await
            .map_err(response_error)?;
        Ok(User {
            login: user.username,
            name: user.name,
            html_url: user.web_url,
            avatar_url: user.avatar_url.unwrap_or_default(),
        })
    }

    pub async fn repository(&self, repo: &RepoRef) -> Result<Repository, ForgeError> {
        #[derive(Deserialize)]
        struct ApiProject {
            name: String,
            path_with_namespace: String,
            web_url: String,
            http_url_to_repo: Option<String>,
            ssh_url_to_repo: Option<String>,
            visibility: String,
            default_branch: Option<String>,
            description: Option<String>,
        }
        let project: ApiProject = self
            .http
            .get(format!(
                "{}/projects/{}",
                self.base_url,
                urlencoding::encode(&format!("{}/{}", repo.owner, repo.name))
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(api_error)?
            .error_for_status()
            .map_err(api_error)?
            .json()
            .await
            .map_err(response_error)?;
        Ok(Repository {
            name: project.name,
            full_name: project.path_with_namespace,
            html_url: project.web_url,
            clone_url: project.http_url_to_repo,
            ssh_url: project.ssh_url_to_repo,
            private: project.visibility == "private",
            default_branch: project.default_branch,
            description: project.description,
        })
    }

    pub async fn create_repository(
        &self,
        request: &CreateRepository,
        organization: Option<&str>,
    ) -> Result<Repository, ForgeError> {
        let mut payload = serde_json::json!({
            "name": request.name,
            "description": request.description,
            "visibility": if request.private { "private" } else { "public" },
            "initialize_with_readme": request.auto_init
        });
        if let Some(group) = organization {
            let group_id = self.group_id(group).await?;
            payload["namespace_id"] = serde_json::json!(group_id);
        }
        let value: serde_json::Value = self
            .http
            .post(format!("{}/projects", self.base_url))
            .bearer_auth(&self.token)
            .json(&payload)
            .send()
            .await
            .map_err(api_error)?
            .error_for_status()
            .map_err(api_error)?
            .json()
            .await
            .map_err(response_error)?;
        self.repository_from_value(value)
    }

    pub async fn delete_repository(&self, repo: &RepoRef) -> Result<(), ForgeError> {
        self.http
            .delete(format!(
                "{}/projects/{}",
                self.base_url,
                self.project_path(repo)
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(api_error)?
            .error_for_status()
            .map(|_| ())
            .map_err(api_error)
    }

    async fn group_id(&self, group: &str) -> Result<u64, ForgeError> {
        #[derive(Deserialize)]
        struct ApiGroup {
            id: u64,
        }
        let group: ApiGroup = self
            .http
            .get(format!(
                "{}/groups/{}",
                self.base_url,
                urlencoding::encode(group)
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(api_error)?
            .error_for_status()
            .map_err(api_error)?
            .json()
            .await
            .map_err(response_error)?;
        Ok(group.id)
    }

    fn project_path(&self, repo: &RepoRef) -> String {
        urlencoding::encode(&repo.to_string()).into_owned()
    }

    fn repository_from_value(&self, value: serde_json::Value) -> Result<Repository, ForgeError> {
        #[derive(Deserialize)]
        struct ApiProject {
            name: String,
            path_with_namespace: String,
            web_url: String,
            http_url_to_repo: Option<String>,
            ssh_url_to_repo: Option<String>,
            visibility: String,
            default_branch: Option<String>,
            description: Option<String>,
        }
        let project: ApiProject = serde_json::from_value(value).map_err(response_error)?;
        Ok(Repository {
            name: project.name,
            full_name: project.path_with_namespace,
            html_url: project.web_url,
            clone_url: project.http_url_to_repo,
            ssh_url: project.ssh_url_to_repo,
            private: project.visibility == "private",
            default_branch: project.default_branch,
            description: project.description,
        })
    }

    /// One page of a list endpoint, with GitLab's `x-total` and
    /// `x-next-page` headers (`x-total` is omitted above 10k items).
    async fn list_page<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        page: u32,
    ) -> Result<Listing<T>, ForgeError> {
        let page = page.max(1);
        let response = self
            .http
            .get(page_query(url, page))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(api_error)?
            .error_for_status()
            .map_err(api_error)?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let more = header("x-next-page").is_some();
        let reported: Option<u64> = header("x-total").and_then(|v| v.parse().ok());
        let items: Vec<T> = response.json().await.map_err(response_error)?;
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
        let url = format!(
            "{}/projects/{}/repository/branches",
            self.base_url,
            self.project_path(repo)
        );
        let listing: Listing<ApiBranch> = self.list_page(&url, page).await?;
        Ok(listing.map(|branch| Branch {
            name: branch.name,
            sha: branch.commit.id,
            protected: branch.protected,
        }))
    }

    pub async fn delete_branch(&self, repo: &RepoRef, branch: &str) -> Result<(), ForgeError> {
        self.http
            .delete(format!(
                "{}/projects/{}/repository/branches/{}",
                self.base_url,
                self.project_path(repo),
                urlencoding::encode(branch)
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(api_error)?
            .error_for_status()
            .map(|_| ())
            .map_err(api_error)
    }

    pub async fn merge_requests(
        &self,
        repo: &RepoRef,
        page: u32,
        state: PullRequestState,
        head: Option<&str>,
        author: Option<&str>,
    ) -> Result<Listing<PullRequest>, ForgeError> {
        let mut url = format!(
            "{}/projects/{}/merge_requests?state={}",
            self.base_url,
            self.project_path(repo),
            state_param(state)
        );
        if let Some(branch) = head {
            url.push_str(&format!("&source_branch={}", urlencoding::encode(branch)));
        }
        if let Some(author) = author {
            url.push_str(&format!("&author_username={}", urlencoding::encode(author)));
        }
        let listing: Listing<ApiMergeRequest> = self.list_page(&url, page).await?;
        Ok(listing.map(|request| request.into_pull_request()))
    }

    pub async fn pull_merge_request(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequest, ForgeError> {
        let value: serde_json::Value = self
            .http
            .get(format!(
                "{}/projects/{}/merge_requests/{}",
                self.base_url,
                self.project_path(repo),
                number
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(api_error)?
            .error_for_status()
            .map_err(api_error)?
            .json()
            .await
            .map_err(response_error)?;
        self.merge_request_from_value(value)
    }

    /// The checks of merge request `iid`: the head pipeline's jobs.
    pub async fn pull_request_checks(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequestChecks, ForgeError> {
        #[derive(Deserialize)]
        struct ApiPipeline {
            id: u64,
        }
        #[derive(Deserialize)]
        struct ApiHeadPipeline {
            head_pipeline: Option<ApiPipeline>,
        }
        #[derive(Deserialize)]
        struct ApiJob {
            name: String,
            status: String,
            web_url: Option<String>,
        }
        let value: serde_json::Value = self
            .http
            .get(format!(
                "{}/projects/{}/merge_requests/{}",
                self.base_url,
                self.project_path(repo),
                number
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(api_error)?
            .error_for_status()
            .map_err(api_error)?
            .json()
            .await
            .map_err(response_error)?;
        let head: ApiHeadPipeline = serde_json::from_value(value).map_err(response_error)?;
        let Some(pipeline) = head.head_pipeline else {
            return Ok(PullRequestChecks {
                rollup: super::types::CheckRollup::None,
                checks: Vec::new(),
            });
        };
        let jobs: Vec<ApiJob> = self
            .http
            .get(format!(
                "{}/projects/{}/pipelines/{}/jobs?per_page=100",
                self.base_url,
                self.project_path(repo),
                pipeline.id
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(api_error)?
            .error_for_status()
            .map_err(api_error)?
            .json()
            .await
            .map_err(response_error)?;
        let statuses = jobs
            .iter()
            .map(|job| normalize_check_status(&job.status).to_owned())
            .collect::<Vec<_>>();
        Ok(PullRequestChecks {
            rollup: rollup_from(statuses),
            checks: jobs
                .into_iter()
                .map(|job| CheckRun {
                    name: job.name,
                    status: normalize_check_status(&job.status).to_owned(),
                    url: job.web_url,
                })
                .collect(),
        })
    }

    pub async fn create_merge_request(
        &self,
        repo: &RepoRef,
        request: &CreatePullRequest,
    ) -> Result<PullRequest, ForgeError> {
        let value: serde_json::Value = self
            .http
            .post(format!(
                "{}/projects/{}/merge_requests",
                self.base_url,
                self.project_path(repo)
            ))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "title": request.title,
                "source_branch": request.head,
                "target_branch": request.base,
                "description": request.body,
                "draft": request.draft
            }))
            .send()
            .await
            .map_err(api_error)?
            .error_for_status()
            .map_err(api_error)?
            .json()
            .await
            .map_err(response_error)?;
        self.merge_request_from_value(value)
    }

    pub async fn retarget_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
        base: &str,
    ) -> Result<PullRequest, ForgeError> {
        let value: serde_json::Value = self
            .http
            .put(format!(
                "{}/projects/{}/merge_requests/{}",
                self.base_url,
                self.project_path(repo),
                number
            ))
            .bearer_auth(&self.token)
            .form(&[("target_branch", base)])
            .send()
            .await
            .map_err(api_error)?
            .error_for_status()
            .map_err(api_error)?
            .json()
            .await
            .map_err(response_error)?;
        self.merge_request_from_value(value)
    }

    pub async fn close_merge_request(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequest, ForgeError> {
        let value: serde_json::Value = self
            .http
            .put(format!(
                "{}/projects/{}/merge_requests/{}",
                self.base_url,
                self.project_path(repo),
                number
            ))
            .bearer_auth(&self.token)
            .form(&[("state_event", "close")])
            .send()
            .await
            .map_err(api_error)?
            .error_for_status()
            .map_err(api_error)?
            .json()
            .await
            .map_err(response_error)?;
        self.merge_request_from_value(value)
    }

    /// Merge request `number` with `method`: a rebase rebases onto the target
    /// first so the merge itself is a fast-forward, a squash squashes, and
    /// the source branch goes away when `delete_branch` is set.
    pub async fn merge_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<PullRequest, ForgeError> {
        if method == MergeMethod::Rebase {
            self.http
                .post(format!(
                    "{}/projects/{}/merge_requests/{}/rebase",
                    self.base_url,
                    self.project_path(repo),
                    number
                ))
                .bearer_auth(&self.token)
                .send()
                .await
                .map_err(api_error)?
                .error_for_status()
                .map_err(api_error)?;
        }
        let mut payload = serde_json::json!({
            "should_remove_source_branch": delete_branch,
        });
        if method == MergeMethod::Squash {
            payload["squash"] = serde_json::json!(true);
        }
        self.http
            .put(format!(
                "{}/projects/{}/merge_requests/{}",
                self.base_url,
                self.project_path(repo),
                number
            ))
            .bearer_auth(&self.token)
            .json(&payload)
            .send()
            .await
            .map_err(api_error)?
            .error_for_status()
            .map_err(api_error)?;
        self.pull_merge_request(repo, number).await
    }

    fn merge_request_from_value(
        &self,
        value: serde_json::Value,
    ) -> Result<PullRequest, ForgeError> {
        let request: ApiMergeRequest = serde_json::from_value(value).map_err(response_error)?;
        Ok(request.into_pull_request())
    }

    /// Reopen merge request `number`.
    pub async fn reopen_merge_request(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequest, ForgeError> {
        let value: serde_json::Value = self
            .http
            .put(format!(
                "{}/projects/{}/merge_requests/{}",
                self.base_url,
                self.project_path(repo),
                number
            ))
            .bearer_auth(&self.token)
            .form(&[("state_event", "reopen")])
            .send()
            .await
            .map_err(api_error)?
            .error_for_status()
            .map_err(api_error)?
            .json()
            .await
            .map_err(response_error)?;
        self.merge_request_from_value(value)
    }

    /// Comment `body` on merge request `number`.
    pub async fn comment_merge_request(
        &self,
        repo: &RepoRef,
        number: u64,
        body: &str,
    ) -> Result<PullRequestComment, ForgeError> {
        #[derive(Deserialize)]
        struct ApiNote {
            id: u64,
            body: String,
            author: Option<ApiAccount>,
            created_at: Option<String>,
        }
        let note: ApiNote = self
            .http
            .post(format!(
                "{}/projects/{}/merge_requests/{}/notes",
                self.base_url,
                self.project_path(repo),
                number
            ))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await
            .map_err(api_error)?
            .error_for_status()
            .map_err(api_error)?
            .json()
            .await
            .map_err(response_error)?;
        Ok(PullRequestComment {
            id: note.id,
            body: note.body,
            author: note.author.map(|user| user.username),
            url: None,
            created_at: note.created_at,
        })
    }

    /// Change the title, description and target branch merge request `number`
    /// carries; each `None` leaves that field alone.
    pub async fn edit_merge_request(
        &self,
        repo: &RepoRef,
        number: u64,
        title: Option<&str>,
        body: Option<&str>,
        base: Option<&str>,
    ) -> Result<PullRequest, ForgeError> {
        let mut payload = serde_json::Map::new();
        if let Some(title) = title {
            payload.insert("title".to_owned(), serde_json::json!(title));
        }
        if let Some(body) = body {
            payload.insert("description".to_owned(), serde_json::json!(body));
        }
        if let Some(base) = base {
            payload.insert("target_branch".to_owned(), serde_json::json!(base));
        }
        let value: serde_json::Value = self
            .http
            .put(format!(
                "{}/projects/{}/merge_requests/{}",
                self.base_url,
                self.project_path(repo),
                number
            ))
            .bearer_auth(&self.token)
            .json(&payload)
            .send()
            .await
            .map_err(api_error)?
            .error_for_status()
            .map_err(api_error)?
            .json()
            .await
            .map_err(response_error)?;
        self.merge_request_from_value(value)
    }

    /// Mark merge request `number` ready or draft by adding or removing the
    /// `Draft: ` title prefix GitLab reads.
    pub async fn set_merge_request_draft(
        &self,
        repo: &RepoRef,
        number: u64,
        draft: bool,
    ) -> Result<PullRequest, ForgeError> {
        let current = self.pull_merge_request(repo, number).await?;
        const PREFIX: &str = "Draft:";
        let drafted = current.title.starts_with(PREFIX);
        if draft == drafted {
            return Ok(current);
        }
        let title = if draft {
            format!("{PREFIX} {}", current.title)
        } else {
            current
                .title
                .trim_start_matches(PREFIX)
                .trim_start()
                .to_owned()
        };
        self.edit_merge_request(repo, number, Some(&title), None, None)
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use super::*;

    #[test]
    fn maps_gitlab_merge_requests() {
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("server address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request");
            let mut request = [0_u8; 2048];
            let bytes = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..bytes]);
            assert!(request.starts_with("GET /api/v4/projects/group%2Fproject/merge_requests"));
            assert!(request.contains("per_page=100&page=2"));
            assert!(request.contains("authorization: Bearer token"));
            let body = r#"[{"iid":7,"title":"Improve forge","state":"opened","web_url":"https://gitlab.example/merge_requests/7","draft":true,"source_branch":"feature","target_branch":"main"}]"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Total: 150\r\nX-Next-Page: \r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });
        let host = format!("http://{address}");
        let client = GitlabClient::from_token("token".to_owned(), Some(&host)).expect("client");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let requests = runtime
            .block_on(client.merge_requests(
                &RepoRef {
                    owner: "group".to_owned(),
                    name: "project".to_owned(),
                },
                2,
                PullRequestState::Open,
                None,
                None,
            ))
            .expect("merge requests");
        server.join().expect("server thread");
        assert_eq!(requests.page, 2);
        assert!(!requests.more);
        assert_eq!(requests.total, Some(150));
        let items = &requests.items;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].number, 7);
        assert_eq!(items[0].head_branch, "feature");
        assert!(items[0].draft);
    }
}
