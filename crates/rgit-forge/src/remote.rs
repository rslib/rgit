//! One parser for the remote URLs of the supported forges.

use super::ForgeError;
use super::types::{Provider, RepoRef};

/// The provider, host and repository behind a Git remote URL.
///
/// `gitlab_host` names a self-hosted GitLab to recognize: a profile host or
/// `GITLAB_HOST`. `host` is the URL's own host, so a caller can connect to the
/// GitLab the remote points at.
pub struct RemoteTarget {
    pub provider: Provider,
    pub host: String,
    pub repo: RepoRef,
}

impl RemoteTarget {
    pub fn parse(url: &str, gitlab_host: Option<&str>) -> Result<Self, ForgeError> {
        let value = url.trim().trim_end_matches(".git");
        let (host, path) = split_authority(value)?;
        let provider = Provider::from_host(host, gitlab_host)
            .ok_or_else(|| ForgeError::UnsupportedHost(host.to_owned()))?;
        Ok(Self {
            provider,
            host: format!("https://{host}"),
            repo: RepoRef::parse(path.trim_start_matches('/'))?,
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
        let target = RemoteTarget::parse("git@github.com:owner/repo.git", None).expect("github");
        assert_eq!(target.provider, Provider::Github);
        assert_eq!(target.host, "https://github.com");
        assert_eq!(target.repo.to_string(), "owner/repo");
    }

    #[test]
    fn parses_nested_gitlab_groups() {
        let target = RemoteTarget::parse("https://gitlab.com/group/subgroup/project.git", None)
            .expect("gitlab");
        assert_eq!(target.provider, Provider::Gitlab);
        assert_eq!(target.repo.owner, "group");
        assert_eq!(target.repo.name, "subgroup/project");
    }

    #[test]
    fn parses_an_ssh_url_with_a_port() {
        let target = RemoteTarget::parse(
            "ssh://git@code.example.com:2222/group/project.git",
            Some("https://code.example.com:2222"),
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
            Some("https://gitlab.internal.example"),
        )
        .expect("configured host");
        assert_eq!(target.provider, Provider::Gitlab);
        assert!(RemoteTarget::parse("https://example.com/a/b", None).is_err());
    }

    #[test]
    fn rejects_a_remote_with_no_path() {
        assert!(RemoteTarget::parse("https://github.com", None).is_err());
    }
}
