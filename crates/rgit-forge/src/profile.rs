//! Forge profile configuration, and the resolution of a forge account and
//! repository from flags, profile and environment.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use super::ForgeError;
use super::remote::RemoteTarget;
use super::types::{Provider, RepoRef};

/// The `[forge]` config table: a default profile and the named profiles.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct ForgeConfig {
    pub default_profile: Option<String>,
    pub profiles: HashMap<String, ForgeProfile>,
}

impl ForgeConfig {
    /// Read `[forge]` from the rgit config file. A file that is not there
    /// means no profiles; one that cannot be read or parsed is an error.
    pub fn load() -> Result<Self, ForgeError> {
        let Some(path) = config_path() else {
            return Ok(Self::default());
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(ForgeError::ConfigRead {
                    path,
                    detail: error.to_string(),
                });
            }
        };
        Self::from_toml(&text).map_err(|error| ForgeError::ConfigParse {
            path,
            detail: error.to_string(),
        })
    }

    fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct File {
            forge: ForgeConfig,
        }
        toml::from_str::<File>(text).map(|file| file.forge)
    }

    pub fn profile(&self, name: &str) -> Option<&ForgeProfile> {
        self.profiles.get(name)
    }

    /// The provider and host of every profile that names both, by profile name.
    fn hosts(&self) -> Vec<(Provider, String)> {
        let mut names: Vec<_> = self.profiles.keys().collect();
        names.sort();
        names
            .into_iter()
            .filter_map(|name| {
                let profile = &self.profiles[name];
                let provider = Provider::parse(profile.provider.as_deref()?).ok()?;
                Some((provider, profile.host.clone()?))
            })
            .collect()
    }
}

/// One `[forge.profiles.<name>]` entry. Credentials stay in the OS keychain.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct ForgeProfile {
    pub provider: Option<String>,
    pub account: Option<String>,
    pub host: Option<String>,
}

/// The rgit config-file path: `RGIT_CONFIG`, else
/// `XDG_CONFIG_HOME/rgit/config.toml`, else `$HOME/.config/rgit/config.toml`.
/// `None` when no location is available.
fn config_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("RGIT_CONFIG") {
        return Some(PathBuf::from(explicit));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("rgit").join("config.toml"))
}

/// The selection of a forge account: the flags a command carries, filled in
/// from the profile it picked.
#[derive(Debug, Clone, Default)]
pub struct ForgeSelection {
    /// The named profile, or the default profile once resolved.
    pub profile: Option<String>,
    /// The provider the profile names. An operation takes its provider from the
    /// repository target or the Git remote, so this stays a name.
    pub provider: Option<String>,
    pub account: Option<String>,
    pub host: Option<String>,
}

impl ForgeSelection {
    /// Fill in the provider, account and host from the named or default
    /// profile. The flags win over the profile.
    pub fn resolve(mut self) -> Result<Self, ForgeError> {
        let config = ForgeConfig::load()?;
        let name = self
            .profile
            .clone()
            .or_else(|| config.default_profile.clone());
        let profile = match name.as_deref() {
            Some(name) => config
                .profile(name)
                .cloned()
                .ok_or_else(|| ForgeError::UnknownProfile(name.to_owned()))?,
            None => ForgeProfile::default(),
        };
        self.profile = name;
        if self.provider.is_none() {
            self.provider = profile.provider;
        }
        if self.account.is_none() {
            self.account = profile.account;
        }
        if self.host.is_none() {
            self.host = profile.host;
        }
        Ok(self)
    }

    /// The hosts that name a provider: the selection itself, every profile,
    /// then the provider environment hosts. A selected host with no provider
    /// is a self-hosted GitLab.
    fn configured_hosts(&self) -> Result<Vec<(Provider, String)>, ForgeError> {
        let mut hosts = Vec::new();
        if let Some(host) = &self.host {
            let provider = match self.provider.as_deref() {
                Some(name) => Provider::parse(name)?,
                None => Provider::Gitlab,
            };
            hosts.push((provider, host.clone()));
        }
        hosts.extend(ForgeConfig::load()?.hosts());
        for provider in [Provider::Gitlab, Provider::Forgejo] {
            if let Some(host) = provider.env_host() {
                hosts.push((provider, host));
            }
        }
        Ok(hosts)
    }

    /// The host this selection names for `provider`: the selected host when
    /// its provider fits, else the host of the first profile of that provider.
    fn host_for(&self, provider: Provider) -> Option<String> {
        let selected = match self.provider.as_deref() {
            Some(name) => Provider::parse(name).ok() == Some(provider),
            None => true,
        };
        if selected && self.host.is_some() {
            return self.host.clone();
        }
        ForgeConfig::load()
            .ok()?
            .hosts()
            .into_iter()
            .find_map(|(candidate, host)| (candidate == provider).then_some(host))
    }

    /// The credential account label to connect with: the flag, the profile,
    /// then `RGIT_FORGE_ACCOUNT`.
    pub fn account(&self) -> String {
        self.account
            .clone()
            .or_else(|| std::env::var("RGIT_FORGE_ACCOUNT").ok())
            .unwrap_or_else(|| "default".to_owned())
    }
}

/// A resolved forge account and the repository it acts on. `repo` is `None` for
/// the operations that name no repository yet, such as creating one.
#[derive(Debug, Clone)]
pub struct ForgeTarget {
    pub provider: Provider,
    pub host: String,
    pub account: String,
    pub repo: Option<RepoRef>,
}

impl ForgeTarget {
    /// Resolve the account and pair it with a repository named on the command
    /// line: `PROVIDER:OWNER/REPO` when the target splits, else `OWNER/REPO`
    /// on GitHub.
    pub fn from_argument(
        selection: &ForgeSelection,
        target: Option<&str>,
        repo: Option<&str>,
    ) -> Result<Self, ForgeError> {
        let (provider, repo) = match (target, repo) {
            (Some(provider), Some(repo)) => (Provider::parse(provider)?, RepoRef::parse(repo)?),
            (Some(target), None) => match target.split_once(':') {
                Some((provider, repo)) => (Provider::parse(provider)?, RepoRef::parse(repo)?),
                None => (Provider::Github, RepoRef::parse(target)?),
            },
            (None, _) => return Err(ForgeError::MissingTarget),
        };
        Ok(Self::new(selection, provider, Some(repo)))
    }

    /// Resolve the account and pair it with the repository a Git remote points
    /// at. The connection host is the flag or profile host, else the provider's
    /// environment host, else the host the remote names, so a self-hosted
    /// GitLab remote connects without extra configuration.
    pub fn from_remote(selection: &ForgeSelection, remote: &str) -> Result<Self, ForgeError> {
        let target = RemoteTarget::parse(remote, &selection.configured_hosts()?)?;
        let host = if target.configured {
            target.host
        } else {
            selection
                .host_for(target.provider)
                .or_else(|| target.provider.env_host())
                .unwrap_or(target.host)
        };
        Ok(Self {
            provider: target.provider,
            host,
            account: selection.account(),
            repo: Some(target.repo),
        })
    }

    /// The repository this target acts on.
    pub fn repo(&self) -> Result<&RepoRef, ForgeError> {
        self.repo.as_ref().ok_or(ForgeError::MissingRepository)
    }

    /// A target for an operation that names no repository yet, such as
    /// creating one. The connection host is the flag or profile host, else the
    /// provider's environment host, else its default.
    pub fn for_provider(selection: &ForgeSelection, provider: Provider) -> Self {
        Self::new(selection, provider, None)
    }

    fn new(selection: &ForgeSelection, provider: Provider, repo: Option<RepoRef>) -> Self {
        let host = selection
            .host_for(provider)
            .or_else(|| provider.env_host())
            .unwrap_or_else(|| provider.default_host().to_owned());
        Self {
            provider,
            host,
            account: selection.account(),
            repo,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_fills_in_the_provider_account_and_host() {
        let config: ForgeConfig = toml::from_str(
            r#"
            default_profile = "personal"
            [profiles.work]
            provider = "gitlab"
            account = "work"
            host = "https://gitlab.example.com"
            [profiles.personal]
            account = "dev"
            "#,
        )
        .expect("parsable profile config");
        let profile = config.profile("work").expect("named profile");
        assert_eq!(profile.provider.as_deref(), Some("gitlab"));
        assert_eq!(profile.account.as_deref(), Some("work"));
        assert_eq!(profile.host.as_deref(), Some("https://gitlab.example.com"));
        assert!(config.profile("nope").is_none());
    }

    #[test]
    fn profiles_are_read_from_the_forge_table() {
        let config = ForgeConfig::from_toml(
            r#"
            [profiles.vim]
            extends = "vim"
            [forge.profiles.lab]
            provider = "forgejo"
            host = "https://git.rs.ht"
            "#,
        )
        .expect("parsable config");
        assert_eq!(config.profiles.len(), 1);
        assert_eq!(
            config.hosts(),
            [(Provider::Forgejo, "https://git.rs.ht".to_owned())]
        );
    }

    #[test]
    fn an_explicit_target_names_the_provider() {
        let selection = ForgeSelection::default();
        let target = ForgeTarget::from_argument(&selection, Some("owner/repo"), None)
            .expect("github target");
        assert_eq!(target.provider, Provider::Github);
        assert_eq!(target.host, "https://github.com");
        assert_eq!(target.account, "default");
        assert_eq!(target.repo().expect("repo").to_string(), "owner/repo");

        let target = ForgeTarget::from_argument(&selection, Some("gitlab:group/project"), None)
            .expect("gitlab target");
        assert_eq!(target.provider, Provider::Gitlab);
        assert_eq!(target.host, "https://gitlab.com");
    }

    #[test]
    fn forgejo_provider_target_ignores_a_gitlab_selection() {
        let selection = ForgeSelection {
            provider: Some("gitlab".to_owned()),
            host: Some("https://gitlab.example.com".to_owned()),
            ..ForgeSelection::default()
        };
        let target = ForgeTarget::from_argument(&selection, Some("forgejo:owner/repo"), None)
            .expect("forgejo target");
        assert_eq!(target.provider, Provider::Forgejo);
        assert_ne!(target.host, "https://gitlab.example.com");
    }

    #[test]
    fn forgejo_provider_remote_uses_a_selected_forgejo_host() {
        let selection = ForgeSelection {
            provider: Some("forgejo".to_owned()),
            host: Some("https://git.rs.ht".to_owned()),
            ..ForgeSelection::default()
        };
        let target = ForgeTarget::from_remote(&selection, "forgejo@git.rs.ht:rayandrew/lab.git")
            .expect("forgejo remote");
        assert_eq!(target.provider, Provider::Forgejo);
        assert_eq!(target.host, "https://git.rs.ht");
        assert_eq!(target.repo().expect("repo").to_string(), "rayandrew/lab");
    }

    #[test]
    fn a_second_positional_needs_a_target_first() {
        let selection = ForgeSelection::default();
        let error = ForgeTarget::from_argument(&selection, None, Some("owner/repo"))
            .expect_err("no target");
        assert!(error.to_string().contains("OWNER/REPO"));
    }

    fn random_name(prefix: &str) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .subsec_nanos();
        format!("{prefix}-{nanos:x}")
    }

    #[test]
    fn a_remote_names_the_provider_and_repository() {
        let account = random_name("account");
        let owner = random_name("owner");
        let selection = ForgeSelection {
            account: Some(account.clone()),
            ..ForgeSelection::default()
        };
        let remote = format!("git@github.com:{owner}/rgit.git");
        let target = ForgeTarget::from_remote(&selection, &remote).expect("remote");
        assert_eq!(target.provider, Provider::Github);
        assert_eq!(target.account, account);
        assert_eq!(target.host, "https://github.com");
        assert_eq!(
            target.repo().expect("repo").to_string(),
            format!("{owner}/rgit")
        );

        let error = ForgeTarget::from_remote(&selection, "https://example.com/a/b")
            .expect_err("unsupported remote");
        assert!(error.to_string().contains("example.com"));
    }

    #[test]
    fn the_selected_host_becomes_the_connection_host() {
        let selection = ForgeSelection {
            host: Some("https://gitlab.example.com".to_owned()),
            ..ForgeSelection::default()
        };
        let target =
            ForgeTarget::from_remote(&selection, "https://gitlab.example.com/g/p").expect("remote");
        assert_eq!(target.provider, Provider::Gitlab);
        assert_eq!(target.host, "https://gitlab.example.com");
    }
}
