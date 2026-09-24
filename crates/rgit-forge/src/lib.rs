use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use base64::Engine;
use keyring::Entry;
use octocrab::Octocrab;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

const KEYRING_SERVICE: &str = "rgit.forge";
pub const GITHUB_OAUTH_CLIENT_ID: &str = "Ov23lizdJOnx6whPL9rw";
#[derive(Debug, Error)]
pub enum ForgeError {
    #[error("forge authentication required; run `rgit forge login <provider>`")]
    AuthenticationRequired,
    #[error("credential store error: {0}")]
    CredentialStore(String),
    #[error("GitHub API error: {0}")]
    Api(String),
    #[error("invalid repository reference {0:?}; expected OWNER/REPO")]
    InvalidRepository(String),
    #[error("invalid response from GitHub: {0}")]
    Response(String),
    #[error("GitHub OAuth error: {0}")]
    OAuth(String),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub html_url: String,
    pub head_branch: String,
    pub base_branch: String,
    pub draft: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CredentialIdentity {
    pub provider: String,
    pub host: String,
    pub account: String,
}

#[derive(Debug, Clone)]
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

/// Common repository and code-review operations supported by forge providers.
#[allow(async_fn_in_trait)]
pub trait ForgeProvider {
    async fn repository(&self, repo: &RepoRef) -> Result<Repository, ForgeError>;

    async fn branches(&self, repo: &RepoRef) -> Result<Vec<Branch>, ForgeError>;

    async fn delete_branch(&self, repo: &RepoRef, branch: &str) -> Result<(), ForgeError>;

    async fn pull_requests(&self, repo: &RepoRef) -> Result<Vec<PullRequest>, ForgeError>;

    async fn create_pull_request(
        &self,
        repo: &RepoRef,
        request: &CreatePullRequest,
    ) -> Result<PullRequest, ForgeError>;

    async fn close_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequest, ForgeError>;

    async fn current_user(&self) -> Result<User, ForgeError>;
}

pub struct GithubClient {
    api: Octocrab,
}

impl GithubClient {
    pub fn from_token(token: String) -> Result<Self, ForgeError> {
        Octocrab::builder()
            .personal_token(token)
            .build()
            .map(|api| Self { api })
            .map_err(|error| ForgeError::Api(error.to_string()))
    }

    pub async fn from_environment() -> Result<Self, ForgeError> {
        let account = std::env::var("RGIT_FORGE_ACCOUNT").unwrap_or_else(|_| "default".to_owned());
        Self::from_environment_for(&account).await
    }

    pub async fn from_environment_for(account: &str) -> Result<Self, ForgeError> {
        if let Ok(token) = std::env::var("GITHUB_TOKEN").or_else(|_| std::env::var("GH_TOKEN")) {
            return Self::from_token(token);
        }
        let credential = load_credential_for("github", "github.com", account)?
            .ok_or(ForgeError::AuthenticationRequired)?;
        let credential = refresh_if_needed(credential, "github", "github.com", account).await?;
        Self::from_token(credential.access_token)
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

    pub async fn branches(&self, repo: &RepoRef) -> Result<Vec<Branch>, ForgeError> {
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
        let page: Vec<ApiBranch> = self
            .api
            .get(
                format!("/repos/{}/{}/branches?per_page=100", repo.owner, repo.name),
                None::<&()>,
            )
            .await
            .map_err(api_error)?;
        Ok(page
            .into_iter()
            .map(|branch| Branch {
                name: branch.name,
                sha: branch.commit.sha,
                protected: branch.protected,
            })
            .collect())
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

    pub async fn pull_requests(&self, repo: &RepoRef) -> Result<Vec<PullRequest>, ForgeError> {
        #[derive(Deserialize)]
        struct ApiRequest {
            number: u64,
            title: String,
            state: String,
            html_url: String,
            draft: bool,
            head: ApiRef,
            base: ApiRef,
        }
        #[derive(Deserialize)]
        struct ApiRef {
            #[serde(rename = "ref")]
            branch: String,
        }
        let requests: Vec<ApiRequest> = self
            .api
            .get(
                format!(
                    "/repos/{}/{}/pulls?state=open&per_page=100",
                    repo.owner, repo.name
                ),
                None::<&()>,
            )
            .await
            .map_err(api_error)?;
        Ok(requests
            .into_iter()
            .map(|request| PullRequest {
                number: request.number,
                title: request.title,
                state: request.state,
                html_url: request.html_url,
                head_branch: request.head.branch,
                base_branch: request.base.branch,
                draft: request.draft,
            })
            .collect())
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
    }
}

impl ForgeProvider for GithubClient {
    async fn repository(&self, repo: &RepoRef) -> Result<Repository, ForgeError> {
        GithubClient::repository(self, repo).await
    }

    async fn branches(&self, repo: &RepoRef) -> Result<Vec<Branch>, ForgeError> {
        GithubClient::branches(self, repo).await
    }

    async fn delete_branch(&self, repo: &RepoRef, branch: &str) -> Result<(), ForgeError> {
        GithubClient::delete_branch(self, repo, branch).await
    }

    async fn pull_requests(&self, repo: &RepoRef) -> Result<Vec<PullRequest>, ForgeError> {
        GithubClient::pull_requests(self, repo).await
    }

    async fn create_pull_request(
        &self,
        repo: &RepoRef,
        request: &CreatePullRequest,
    ) -> Result<PullRequest, ForgeError> {
        GithubClient::create_pull_request(self, repo, request).await
    }

    async fn close_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequest, ForgeError> {
        GithubClient::close_pull_request(self, repo, number).await
    }

    async fn current_user(&self) -> Result<User, ForgeError> {
        GithubClient::current_user(self).await
    }
}

pub struct GitlabClient {
    http: reqwest::Client,
    token: String,
    base_url: String,
}

impl GitlabClient {
    pub fn from_token(token: String, host: Option<&str>) -> Result<Self, ForgeError> {
        let host = host.unwrap_or("https://gitlab.com").trim_end_matches('/');
        let base_url = format!("{host}/api/v4");
        reqwest::Url::parse(&base_url).map_err(|error| ForgeError::Api(error.to_string()))?;
        Ok(Self {
            http: reqwest::Client::new(),
            token,
            base_url,
        })
    }

    pub fn from_environment() -> Result<Self, ForgeError> {
        let host = std::env::var("GITLAB_HOST").unwrap_or_else(|_| "https://gitlab.com".to_owned());
        let account = std::env::var("RGIT_FORGE_ACCOUNT").unwrap_or_else(|_| "default".to_owned());
        Self::from_environment_for(&host, &account)
    }

    pub fn from_environment_for(host: &str, account: &str) -> Result<Self, ForgeError> {
        let token =
            match std::env::var("GITLAB_TOKEN").or_else(|_| std::env::var("GITLAB_ACCESS_TOKEN")) {
                Ok(token) => token,
                Err(_) => load_credential_for("gitlab", host, account)?
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
            .map_err(|error| ForgeError::Api(error.to_string()))?
            .error_for_status()
            .map_err(|error| ForgeError::Api(error.to_string()))?
            .json()
            .await
            .map_err(|error| ForgeError::Response(error.to_string()))?;
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
            .map_err(|error| ForgeError::Api(error.to_string()))?
            .error_for_status()
            .map_err(|error| ForgeError::Api(error.to_string()))?
            .json()
            .await
            .map_err(|error| ForgeError::Response(error.to_string()))?;
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
            .map_err(|error| ForgeError::Api(error.to_string()))?
            .error_for_status()
            .map_err(|error| ForgeError::Api(error.to_string()))?
            .json()
            .await
            .map_err(|error| ForgeError::Response(error.to_string()))?;
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
            .map_err(|error| ForgeError::Api(error.to_string()))?
            .error_for_status()
            .map(|_| ())
            .map_err(|error| ForgeError::Api(error.to_string()))
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
            .map_err(|error| ForgeError::Api(error.to_string()))?
            .error_for_status()
            .map_err(|error| ForgeError::Api(error.to_string()))?
            .json()
            .await
            .map_err(|error| ForgeError::Response(error.to_string()))?;
        Ok(group.id)
    }

    fn project_path(&self, repo: &RepoRef) -> String {
        urlencoding::encode(&format!("{}/{}", repo.owner, repo.name)).into_owned()
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
        let project: ApiProject = serde_json::from_value(value)
            .map_err(|error| ForgeError::Response(error.to_string()))?;
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

    pub async fn branches(&self, repo: &RepoRef) -> Result<Vec<Branch>, ForgeError> {
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
        let branches: Vec<ApiBranch> = self
            .http
            .get(format!(
                "{}/projects/{}/repository/branches?per_page=100",
                self.base_url,
                self.project_path(repo)
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|error| ForgeError::Api(error.to_string()))?
            .error_for_status()
            .map_err(|error| ForgeError::Api(error.to_string()))?
            .json()
            .await
            .map_err(|error| ForgeError::Response(error.to_string()))?;
        Ok(branches
            .into_iter()
            .map(|branch| Branch {
                name: branch.name,
                sha: branch.commit.id,
                protected: branch.protected,
            })
            .collect())
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
            .map_err(|error| ForgeError::Api(error.to_string()))?
            .error_for_status()
            .map(|_| ())
            .map_err(|error| ForgeError::Api(error.to_string()))
    }

    pub async fn merge_requests(&self, repo: &RepoRef) -> Result<Vec<PullRequest>, ForgeError> {
        #[derive(Deserialize)]
        struct ApiMergeRequest {
            iid: u64,
            title: String,
            state: String,
            web_url: String,
            draft: bool,
            source_branch: String,
            target_branch: String,
        }
        let requests: Vec<ApiMergeRequest> = self
            .http
            .get(format!(
                "{}/projects/{}/merge_requests?state=opened&per_page=100",
                self.base_url,
                self.project_path(repo)
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|error| ForgeError::Api(error.to_string()))?
            .error_for_status()
            .map_err(|error| ForgeError::Api(error.to_string()))?
            .json()
            .await
            .map_err(|error| ForgeError::Response(error.to_string()))?;
        Ok(requests
            .into_iter()
            .map(|request| PullRequest {
                number: request.iid,
                title: request.title,
                state: request.state,
                html_url: request.web_url,
                head_branch: request.source_branch,
                base_branch: request.target_branch,
                draft: request.draft,
            })
            .collect())
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
            .map_err(|error| ForgeError::Api(error.to_string()))?
            .error_for_status()
            .map_err(|error| ForgeError::Api(error.to_string()))?
            .json()
            .await
            .map_err(|error| ForgeError::Response(error.to_string()))?;
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
            .map_err(|error| ForgeError::Api(error.to_string()))?
            .error_for_status()
            .map_err(|error| ForgeError::Api(error.to_string()))?
            .json()
            .await
            .map_err(|error| ForgeError::Response(error.to_string()))?;
        self.merge_request_from_value(value)
    }

    fn merge_request_from_value(
        &self,
        value: serde_json::Value,
    ) -> Result<PullRequest, ForgeError> {
        #[derive(Deserialize)]
        struct ApiMergeRequest {
            iid: u64,
            title: String,
            state: String,
            web_url: String,
            #[serde(default)]
            draft: bool,
            source_branch: String,
            target_branch: String,
        }
        let request: ApiMergeRequest = serde_json::from_value(value)
            .map_err(|error| ForgeError::Response(error.to_string()))?;
        Ok(PullRequest {
            number: request.iid,
            title: request.title,
            state: request.state,
            html_url: request.web_url,
            head_branch: request.source_branch,
            base_branch: request.target_branch,
            draft: request.draft,
        })
    }
}

impl ForgeProvider for GitlabClient {
    async fn repository(&self, repo: &RepoRef) -> Result<Repository, ForgeError> {
        GitlabClient::repository(self, repo).await
    }

    async fn branches(&self, repo: &RepoRef) -> Result<Vec<Branch>, ForgeError> {
        GitlabClient::branches(self, repo).await
    }

    async fn delete_branch(&self, repo: &RepoRef, branch: &str) -> Result<(), ForgeError> {
        GitlabClient::delete_branch(self, repo, branch).await
    }

    async fn pull_requests(&self, repo: &RepoRef) -> Result<Vec<PullRequest>, ForgeError> {
        GitlabClient::merge_requests(self, repo).await
    }

    async fn create_pull_request(
        &self,
        repo: &RepoRef,
        request: &CreatePullRequest,
    ) -> Result<PullRequest, ForgeError> {
        GitlabClient::create_merge_request(self, repo, request).await
    }

    async fn close_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequest, ForgeError> {
        GitlabClient::close_merge_request(self, repo, number).await
    }

    async fn current_user(&self) -> Result<User, ForgeError> {
        GitlabClient::current_user(self).await
    }
}

#[derive(Debug, Deserialize)]
struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct AccessToken {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DeviceFlowConfig<'a> {
    pub provider: &'a str,
    pub client_id: &'a str,
    pub authorize_endpoint: &'a str,
    pub device_endpoint: &'a str,
    pub token_endpoint: &'a str,
    pub scope: &'a str,
}

pub async fn login_browser(config: DeviceFlowConfig<'_>) -> Result<(), ForgeError> {
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|error| ForgeError::OAuth(error.to_string()))?;
    let port = listener
        .local_addr()
        .map_err(|error| ForgeError::OAuth(error.to_string()))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let mut verifier_bytes = [0_u8; 32];
    rand::rng().fill(&mut verifier_bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_bytes);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let mut state_bytes = [0_u8; 32];
    rand::rng().fill(&mut state_bytes);
    let state = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(state_bytes);
    let authorize_url = format!(
        "{}?client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        config.authorize_endpoint,
        urlencoding::encode(config.client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(config.scope),
        urlencoding::encode(&state),
        urlencoding::encode(&challenge),
    );

    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = listener
            .incoming()
            .next()
            .ok_or_else(|| "callback listener closed".to_owned())
            .and_then(|stream| stream.map_err(|error| error.to_string()))
            .and_then(|mut stream| {
                let mut request = [0_u8; 4096];
                let size = stream.read(&mut request).map_err(|error| error.to_string())?;
                let request = String::from_utf8_lossy(&request[..size]);
                let target = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .ok_or_else(|| "invalid OAuth callback request".to_owned())?;
                let query = target.split_once('?').map(|(_, query)| query).unwrap_or("");
                let mut code = None;
                let mut returned_state = None;
                let mut error = None;
                for pair in query.split('&') {
                    let Some((key, value)) = pair.split_once('=') else {
                        continue;
                    };
                    let value = urlencoding::decode(value)
                        .map_err(|error| error.to_string())?
                        .into_owned();
                    match key {
                        "code" => code = Some(value),
                        "state" => returned_state = Some(value),
                        "error" => error = Some(value),
                        _ => {}
                    }
                }
                let response = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nrgit login complete; return to the terminal.";
                let _ = stream.write_all(response);
                if let Some(error) = error {
                    return Err(error);
                }
                Ok((code.ok_or_else(|| "OAuth callback missing code".to_owned())?,
                    returned_state.ok_or_else(|| "OAuth callback missing state".to_owned())?))
            });
        let _ = sender.send(result);
    });

    println!("Opening GitHub authorization in your browser.");
    println!("If it does not open, visit {authorize_url}");
    let _ = open::that_detached(&authorize_url);
    let (code, returned_state) = receiver
        .recv_timeout(std::time::Duration::from_secs(600))
        .map_err(|error| ForgeError::OAuth(error.to_string()))?
        .map_err(ForgeError::OAuth)?;
    if returned_state != state {
        return Err(ForgeError::OAuth(
            "OAuth state verification failed".to_owned(),
        ));
    }

    let response: AccessToken = reqwest::Client::new()
        .post(config.token_endpoint)
        .header("Accept", "application/json")
        .form(&[
            ("client_id", config.client_id),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await
        .map_err(|error| ForgeError::OAuth(error.to_string()))?
        .json()
        .await
        .map_err(|error| ForgeError::OAuth(error.to_string()))?;
    let token = response.access_token.ok_or_else(|| {
        ForgeError::OAuth(
            response
                .error_description
                .or(response.error)
                .unwrap_or_else(|| "GitHub did not return an access token".to_owned()),
        )
    })?;
    let now = unix_now()?;
    save_credential(
        config.provider,
        &StoredCredential {
            access_token: token,
            refresh_token: response.refresh_token,
            access_expires_at: response.expires_in.map(|seconds| now + seconds),
            refresh_expires_at: None,
        },
    )
}
/// Authenticate through a provider's OAuth device flow and save its token.
pub async fn login_device(config: DeviceFlowConfig<'_>) -> Result<(), ForgeError> {
    if config.client_id.trim().is_empty() {
        return Err(ForgeError::OAuth(format!(
            "{} client ID is empty",
            config.provider
        )));
    }
    let http = reqwest::Client::new();
    let device: DeviceCode = http
        .post(config.device_endpoint)
        .header("Accept", "application/json")
        .form(&[("client_id", config.client_id), ("scope", config.scope)])
        .send()
        .await
        .map_err(|error| ForgeError::OAuth(error.to_string()))?
        .error_for_status()
        .map_err(|error| ForgeError::OAuth(error.to_string()))?
        .json()
        .await
        .map_err(|error| ForgeError::OAuth(error.to_string()))?;

    println!(
        "Open {} and enter code {}.",
        device.verification_uri, device.user_code
    );
    let _ = open::that_detached(&device.verification_uri);

    let mut interval = device.interval.max(5);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(device.expires_in);
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(ForgeError::OAuth("device code expired".to_owned()));
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        let response: AccessToken = http
            .post(config.token_endpoint)
            .header("Accept", "application/json")
            .form(&[
                ("client_id", config.client_id),
                ("device_code", device.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await
            .map_err(|error| ForgeError::OAuth(error.to_string()))?
            .json()
            .await
            .map_err(|error| ForgeError::OAuth(error.to_string()))?;
        match response.error.as_deref() {
            None => {
                let token = response.access_token.ok_or_else(|| {
                    ForgeError::OAuth("provider did not return an access token".to_owned())
                })?;
                let now = unix_now()?;
                save_credential(
                    config.provider,
                    &StoredCredential {
                        access_token: token,
                        refresh_token: response.refresh_token,
                        access_expires_at: response.expires_in.map(|seconds| now + seconds),
                        refresh_expires_at: None,
                    },
                )?;
                return Ok(());
            }
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            Some(error) => {
                return Err(ForgeError::OAuth(
                    response
                        .error_description
                        .unwrap_or_else(|| error.to_owned()),
                ));
            }
        }
    }
}

fn api_error(error: octocrab::Error) -> ForgeError {
    let message = error.to_string();
    if message == "GitHub" {
        ForgeError::Api(format!("{message}: {error:?}"))
    } else {
        ForgeError::Api(message)
    }
}

fn unix_now() -> Result<u64, ForgeError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| ForgeError::OAuth(error.to_string()))
}

fn credential_entry(provider: &str) -> Result<Entry, ForgeError> {
    Entry::new(KEYRING_SERVICE, provider)
        .map_err(|error| ForgeError::CredentialStore(error.to_string()))
}

async fn refresh_if_needed(
    mut credential: StoredCredential,
    provider: &str,
    host: &str,
    account: &str,
) -> Result<StoredCredential, ForgeError> {
    let Some(expires_at) = credential.access_expires_at else {
        return Ok(credential);
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| ForgeError::OAuth(error.to_string()))?
        .as_secs();
    if expires_at > now + 60 {
        return Ok(credential);
    }
    let Some(refresh_token) = credential.refresh_token.clone() else {
        return Err(ForgeError::AuthenticationRequired);
    };
    let response: AccessToken = reqwest::Client::new()
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .form(&[
            ("client_id", GITHUB_OAUTH_CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
        ])
        .send()
        .await
        .map_err(|error| ForgeError::OAuth(error.to_string()))?
        .json()
        .await
        .map_err(|error| ForgeError::OAuth(error.to_string()))?;
    let access_token = response.access_token.ok_or_else(|| {
        ForgeError::OAuth(
            response
                .error_description
                .or(response.error)
                .unwrap_or_else(|| "token refresh failed".to_owned()),
        )
    })?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| ForgeError::OAuth(error.to_string()))?
        .as_secs();
    credential.access_token = access_token;
    credential.access_expires_at = response.expires_in.map(|seconds| now + seconds);
    if response.refresh_token.is_some() {
        credential.refresh_token = response.refresh_token;
    }
    save_credential_for(provider, host, account, &credential)?;
    Ok(credential)
}

pub fn load_provider_token(provider: &str) -> Result<Option<String>, ForgeError> {
    match credential_entry(provider)?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(ForgeError::CredentialStore(error.to_string())),
    }
}

pub fn save_provider_token(provider: &str, token: &str) -> Result<(), ForgeError> {
    if token.trim().is_empty() {
        return Err(ForgeError::CredentialStore(
            "refusing to store an empty token".to_owned(),
        ));
    }
    credential_entry(provider)?
        .set_password(token)
        .map_err(|error| ForgeError::CredentialStore(error.to_string()))
}
pub fn load_credential(provider: &str) -> Result<Option<StoredCredential>, ForgeError> {
    let Some(value) = load_provider_token(provider)? else {
        return Ok(None);
    };
    match serde_json::from_str(&value) {
        Ok(credential) => Ok(Some(credential)),
        Err(_) => Ok(Some(StoredCredential {
            access_token: value,
            refresh_token: None,
            access_expires_at: None,
            refresh_expires_at: None,
        })),
    }
}

fn credential_key(provider: &str, host: &str, account: &str) -> String {
    format!(
        "{}|{}|{}",
        provider.trim().to_ascii_lowercase(),
        host.trim().trim_end_matches('/').to_ascii_lowercase(),
        account.trim()
    )
}

fn identity_index_key(provider: &str) -> String {
    format!("{}|accounts", provider.trim().to_ascii_lowercase())
}

fn load_identity_index(provider: &str) -> Result<Vec<CredentialIdentity>, ForgeError> {
    let value = match credential_entry(&identity_index_key(provider))?.get_password() {
        Ok(value) => value,
        Err(keyring::Error::NoEntry) => return Ok(Vec::new()),
        Err(error) => return Err(ForgeError::CredentialStore(error.to_string())),
    };
    serde_json::from_str(&value).map_err(|error| ForgeError::CredentialStore(error.to_string()))
}

fn save_identity_index(
    provider: &str,
    identities: &[CredentialIdentity],
) -> Result<(), ForgeError> {
    let value = serde_json::to_string(identities)
        .map_err(|error| ForgeError::CredentialStore(error.to_string()))?;
    credential_entry(&identity_index_key(provider))?
        .set_password(&value)
        .map_err(|error| ForgeError::CredentialStore(error.to_string()))
}

pub fn credential_identities(provider: &str) -> Result<Vec<CredentialIdentity>, ForgeError> {
    let mut identities = load_identity_index(provider)?;
    if identities.is_empty() && load_provider_token(provider)?.is_some() {
        identities.push(CredentialIdentity {
            provider: provider.to_ascii_lowercase(),
            host: if provider.eq_ignore_ascii_case("github") {
                "https://github.com".to_owned()
            } else {
                "https://gitlab.com".to_owned()
            },
            account: "default".to_owned(),
        });
    }
    Ok(identities)
}

pub fn load_credential_for(
    provider: &str,
    host: &str,
    account: &str,
) -> Result<Option<StoredCredential>, ForgeError> {
    let key = credential_key(provider, host, account);
    let value = match credential_entry(&key)?.get_password() {
        Ok(value) => Some(value),
        Err(keyring::Error::NoEntry) if account == "default" => load_provider_token(provider)?,
        Err(keyring::Error::NoEntry) => None,
        Err(error) => return Err(ForgeError::CredentialStore(error.to_string())),
    };
    let Some(value) = value else {
        return Ok(None);
    };
    match serde_json::from_str(&value) {
        Ok(credential) => Ok(Some(credential)),
        Err(_) => Ok(Some(StoredCredential {
            access_token: value,
            refresh_token: None,
            access_expires_at: None,
            refresh_expires_at: None,
        })),
    }
}

pub fn save_credential_for(
    provider: &str,
    host: &str,
    account: &str,
    credential: &StoredCredential,
) -> Result<(), ForgeError> {
    let value = serde_json::to_string(credential)
        .map_err(|error| ForgeError::CredentialStore(error.to_string()))?;
    credential_entry(&credential_key(provider, host, account))?
        .set_password(&value)
        .map_err(|error| ForgeError::CredentialStore(error.to_string()))?;
    let mut identities = load_identity_index(provider)?;
    let identity = CredentialIdentity {
        provider: provider.to_ascii_lowercase(),
        host: host.trim().trim_end_matches('/').to_owned(),
        account: account.to_owned(),
    };
    if !identities.contains(&identity) {
        identities.push(identity);
        save_identity_index(provider, &identities)?;
    }
    Ok(())
}

pub fn delete_credential_for(provider: &str, host: &str, account: &str) -> Result<(), ForgeError> {
    match credential_entry(&credential_key(provider, host, account))?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(error) => return Err(ForgeError::CredentialStore(error.to_string())),
    }
    let identities = load_identity_index(provider)?
        .into_iter()
        .filter(|identity| {
            !(identity
                .host
                .eq_ignore_ascii_case(host.trim().trim_end_matches('/'))
                && identity.account == account)
        })
        .collect::<Vec<_>>();
    save_identity_index(provider, &identities)
}
pub fn save_credential(provider: &str, credential: &StoredCredential) -> Result<(), ForgeError> {
    let value = serde_json::to_string(credential)
        .map_err(|error| ForgeError::CredentialStore(error.to_string()))?;
    save_provider_token(provider, &value)
}

pub fn delete_provider_token(provider: &str) -> Result<(), ForgeError> {
    match credential_entry(provider)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(ForgeError::CredentialStore(error.to_string())),
    }
}

pub fn delete_token() -> Result<(), ForgeError> {
    delete_provider_token("github")
}

pub fn read_token_stdin() -> Result<Zeroizing<String>, ForgeError> {
    let mut token = Zeroizing::new(String::new());
    io::stdin()
        .read_to_string(&mut token)
        .map_err(|error| ForgeError::CredentialStore(error.to_string()))?;
    let token = token.trim().to_owned();
    if token.is_empty() {
        return Err(ForgeError::CredentialStore(
            "token input was empty".to_owned(),
        ));
    }
    Ok(Zeroizing::new(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repository_reference() {
        let repo = RepoRef::parse("owner/project.git").expect("valid repository");
        assert_eq!(repo.owner, "owner");
        assert_eq!(repo.name, "project");
        assert!(RepoRef::parse("owner").is_err());
    }

    #[test]
    fn clients_implement_forge_provider() {
        fn assert_provider<T: ForgeProvider>() {}

        assert_provider::<GithubClient>();
        assert_provider::<GitlabClient>();
    }

    #[test]
    fn stored_credentials_round_trip() {
        let credential = StoredCredential {
            access_token: "access".to_owned(),
            refresh_token: Some("refresh".to_owned()),
            access_expires_at: Some(42),
            refresh_expires_at: Some(84),
        };
        let encoded = serde_json::to_string(&credential).expect("serializable");
        let decoded: StoredCredential = serde_json::from_str(&encoded).expect("deserializable");
        assert_eq!(decoded.access_token, "access");
        assert_eq!(decoded.refresh_token.as_deref(), Some("refresh"));
        assert_eq!(decoded.access_expires_at, Some(42));
        assert_eq!(decoded.refresh_expires_at, Some(84));
    }

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
            assert!(request.contains("authorization: Bearer token"));
            let body = r#"[{"iid":7,"title":"Improve forge","state":"opened","web_url":"https://gitlab.example/merge_requests/7","draft":true,"source_branch":"feature","target_branch":"main"}]"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
            .block_on(<GitlabClient as ForgeProvider>::pull_requests(
                &client,
                &RepoRef {
                    owner: "group".to_owned(),
                    name: "project".to_owned(),
                },
            ))
            .expect("merge requests");
        server.join().expect("server thread");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].number, 7);
        assert_eq!(requests[0].head_branch, "feature");
        assert!(requests[0].draft);
    }
}
