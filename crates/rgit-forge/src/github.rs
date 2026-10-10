use octocrab::Octocrab;
use serde::{Deserialize, Serialize};

use super::ForgeError;
use super::auth::{load_credential_for, refresh_if_needed};
use super::types::{
    Branch, CreatePullRequest, CreateRepository, Listing, Provider, PullRequest, RepoRef,
    Repository, User, page_query, total_from_last_page,
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

pub struct GithubClient {
    api: Octocrab,
}

impl GithubClient {
    pub fn from_token(token: String) -> Result<Self, ForgeError> {
        Octocrab::builder()
            .personal_token(token)
            .build()
            .map(|api| Self { api })
            .map_err(api_error)
    }

    pub async fn from_environment() -> Result<Self, ForgeError> {
        let account = std::env::var("RGIT_FORGE_ACCOUNT").unwrap_or_else(|_| "default".to_owned());
        Self::from_environment_for(&account).await
    }

    pub async fn from_environment_for(account: &str) -> Result<Self, ForgeError> {
        if let Some(token) = Provider::Github.env_token() {
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
    ) -> Result<Listing<PullRequest>, ForgeError> {
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
        let listing: Listing<ApiRequest> = self
            .list_page(
                &format!("/repos/{}/{}/pulls?state=open", repo.owner, repo.name),
                page,
            )
            .await?;
        Ok(listing.map(|request| PullRequest {
            number: request.number,
            title: request.title,
            state: request.state,
            html_url: request.html_url,
            head_branch: request.head.branch,
            base_branch: request.base.branch,
            draft: request.draft,
        }))
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
