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
    fn last_page_implies_the_total() {
        assert_eq!(total_from_last_page(1, 0), Some(0));
        assert_eq!(total_from_last_page(1, 42), Some(42));
        assert_eq!(total_from_last_page(4, 7), Some(307));
        assert_eq!(total_from_last_page(5, 0), None);
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
