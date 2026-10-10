//! Native forge integration: GitHub and GitLab API clients, one remote parser
//! and one profile loader, behind a single [`Forge`] client.

mod auth;
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
pub use github::GithubClient;
pub use gitlab::GitlabClient;
pub use profile::{ForgeConfig, ForgeProfile, ForgeSelection, ForgeTarget};
pub use remote::RemoteTarget;
pub use types::{
    Branch, CreatePullRequest, CreateRepository, CredentialIdentity, Listing, PAGE_SIZE, Provider,
    PullRequest, PullRequestState, RepoRef, Repository, StoredCredential, User,
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
}

impl Forge {
    /// Connect to the forge of `target` with its account and host.
    pub async fn connect(target: &ForgeTarget) -> Result<Self, ForgeError> {
        match target.provider {
            Provider::Github => Ok(Self::Github(
                GithubClient::from_environment_for(&target.account).await?,
            )),
            Provider::Gitlab => Ok(Self::Gitlab(GitlabClient::from_environment_for(
                &target.host,
                &target.account,
            )?)),
        }
    }

    pub fn provider(&self) -> Provider {
        match self {
            Self::Github(_) => Provider::Github,
            Self::Gitlab(_) => Provider::Gitlab,
        }
    }

    pub async fn repository(&self, repo: &RepoRef) -> Result<Repository, ForgeError> {
        match self {
            Self::Github(client) => client.repository(repo).await,
            Self::Gitlab(client) => client.repository(repo).await,
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
        }
    }

    pub async fn delete_repository(&self, repo: &RepoRef) -> Result<(), ForgeError> {
        match self {
            Self::Github(client) => client.delete_repository(repo).await,
            Self::Gitlab(client) => client.delete_repository(repo).await,
        }
    }

    pub async fn branches(&self, repo: &RepoRef, page: u32) -> Result<Listing<Branch>, ForgeError> {
        match self {
            Self::Github(client) => client.branches(repo, page).await,
            Self::Gitlab(client) => client.branches(repo, page).await,
        }
    }

    pub async fn delete_branch(&self, repo: &RepoRef, branch: &str) -> Result<(), ForgeError> {
        match self {
            Self::Github(client) => client.delete_branch(repo, branch).await,
            Self::Gitlab(client) => client.delete_branch(repo, branch).await,
        }
    }

    pub async fn pull_requests(
        &self,
        repo: &RepoRef,
        page: u32,
        state: PullRequestState,
        head: Option<&str>,
    ) -> Result<Listing<PullRequest>, ForgeError> {
        match self {
            Self::Github(client) => client.pull_requests(repo, page, state, head).await,
            Self::Gitlab(client) => client.merge_requests(repo, page, state, head).await,
        }
    }

    pub async fn create_pull_request(
        &self,
        repo: &RepoRef,
        request: &CreatePullRequest,
    ) -> Result<PullRequest, ForgeError> {
        match self {
            Self::Github(client) => client.create_pull_request(repo, request).await,
            Self::Gitlab(client) => client.create_merge_request(repo, request).await,
        }
    }

    pub async fn close_pull_request(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> Result<PullRequest, ForgeError> {
        match self {
            Self::Github(client) => client.close_pull_request(repo, number).await,
            Self::Gitlab(client) => client.close_merge_request(repo, number).await,
        }
    }

    pub async fn current_user(&self) -> Result<User, ForgeError> {
        match self {
            Self::Github(client) => client.current_user().await,
            Self::Gitlab(client) => client.current_user().await,
        }
    }
}
