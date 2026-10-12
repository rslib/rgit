//! One parser for the remote URLs of the supported forges.

use super::ForgeError;
use super::types::{Provider, RepoRef};

/// The provider, host and repository behind a Git remote URL.
///
/// `configured` pairs a provider with a host a profile or the environment
/// names, so a self-hosted GitLab or Forgejo is recognized. `host` is the host
/// to connect to: the configured one when the remote matched it, else
/// `https://` and the URL's own host.
pub struct RemoteTarget {
    pub provider: Provider,
    pub host: String,
    pub repo: RepoRef,
    pub configured: bool,
}

impl RemoteTarget {
    pub fn parse(url: &str, configured: &[(Provider, String)]) -> Result<Self, ForgeError> {
        let value = url.trim().trim_end_matches(".git");
        let (host, path) = split_authority(value)?;
        let matched = Provider::configured_match(host, configured);
        let provider = Provider::from_host(host, configured)
            .ok_or_else(|| ForgeError::UnsupportedHost(host.to_owned()))?;
        let connect_host = match matched {
            Some((_, value)) if value.contains("://") => value.trim_end_matches('/').to_owned(),
            Some((_, value)) => format!("https://{}", value.trim().trim_end_matches('/')),
            None => format!("https://{host}"),
        };
        Ok(Self {
            provider,
            host: connect_host,
            repo: RepoRef::parse(path.trim_start_matches('/'))?,
            configured: matched.is_some(),
        })
    }
}

/// Split a remote URL into its host and path. Both `user@host:path` and
/// `scheme://authority/path` are accepted, with or without credentials, user
/// and port.
fn split_authority(url: &str) -> Result<(&str, &str), ForgeError> {
    let after_scheme = url.split_once("://");
    let (authority, path) = match after_scheme {
        Some((_, rest)) => rest
            .split_once('/')
            .ok_or_else(|| ForgeError::InvalidRemote(url.to_owned()))?,
        None => {
            let (authority, path) = url
                .split_once(':')
                .ok_or_else(|| ForgeError::InvalidRemote(url.to_owned()))?;
            (authority, path)
        }
    };
    let host = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    Ok((host, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_remote_urls() {
        let target = RemoteTarget::parse("git@github.com:owner/repo.git", &[]).expect("github");
        assert_eq!(target.provider, Provider::Github);
        assert_eq!(target.host, "https://github.com");
        assert_eq!(target.repo.to_string(), "owner/repo");
    }

    #[test]
    fn parses_nested_gitlab_groups() {
        let target = RemoteTarget::parse("https://gitlab.com/group/subgroup/project.git", &[])
            .expect("gitlab");
        assert_eq!(target.provider, Provider::Gitlab);
        assert_eq!(target.repo.owner, "group");
        assert_eq!(target.repo.name, "subgroup/project");
    }

    #[test]
    fn parses_an_ssh_url_with_a_port() {
        let target = RemoteTarget::parse(
            "ssh://git@code.example.com:2222/group/project.git",
            &[(Provider::Gitlab, "https://code.example.com:2222".to_owned())],
        )
        .expect("self hosted gitlab");
        assert_eq!(target.provider, Provider::Gitlab);
        assert_eq!(target.host, "https://code.example.com:2222");
        assert_eq!(target.repo.to_string(), "group/project");
    }

    #[test]
    fn recognizes_a_self_hosted_gitlab_by_configuration() {
        let target = RemoteTarget::parse(
            "https://gitlab.internal.example/group/project",
            &[(
                Provider::Gitlab,
                "https://gitlab.internal.example".to_owned(),
            )],
        )
        .expect("configured host");
        assert_eq!(target.provider, Provider::Gitlab);
        assert!(RemoteTarget::parse("https://example.com/a/b", &[]).is_err());
    }

    #[test]
    fn forgejo_provider_recognizes_an_ssh_remote_by_profile_host() {
        let configured = [(Provider::Forgejo, "https://git.rs.ht".to_owned())];
        let target = RemoteTarget::parse("forgejo@git.rs.ht:rayandrew/lab.git", &configured)
            .expect("forgejo remote");
        assert_eq!(target.provider, Provider::Forgejo);
        assert_eq!(target.host, "https://git.rs.ht");
        assert_eq!(target.repo.to_string(), "rayandrew/lab");
        assert!(target.configured);
    }

    #[test]
    fn forgejo_provider_leaves_an_unconfigured_host_unsupported() {
        assert!(RemoteTarget::parse("forgejo@git.rs.ht:rayandrew/lab.git", &[]).is_err());
    }

    #[test]
    fn rejects_a_remote_with_no_path() {
        assert!(RemoteTarget::parse("https://github.com", &[]).is_err());
    }
}
