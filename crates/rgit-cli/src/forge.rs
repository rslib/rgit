use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use crate::cli::{AuthCmd, ForgeBranchCmd, ForgeCmd, PrCmd, RepoCmd};
use anyhow::{Result, ensure};
use rgit_forge::{
    CreatePullRequest, CreateRepository, ForgeError, GithubClient, GitlabClient, RepoRef,
};
use serde::{Deserialize, Serialize};
#[derive(Clone, Default)]
pub struct ForgeContext {
    pub profile: Option<String>,
    pub provider: Option<String>,
    pub account: Option<String>,
    pub host: Option<String>,
}

static FORGE_CONTEXT: LazyLock<Mutex<ForgeContext>> =
    LazyLock::new(|| Mutex::new(ForgeContext::default()));

fn forge_context() -> ForgeContext {
    FORGE_CONTEXT.lock().expect("forge context mutex").clone()
}
#[derive(Debug, Deserialize, Default)]
struct ForgeConfigFile {
    #[serde(default)]
    forge: ForgeConfig,
}

#[derive(Debug, Deserialize, Default)]
struct ForgeConfig {
    #[serde(default)]
    default_profile: Option<String>,
    #[serde(default)]
    profiles: HashMap<String, ForgeProfile>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct ForgeProfile {
    provider: Option<String>,
    account: Option<String>,
    host: Option<String>,
}
fn config_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("RGIT_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    if let Some(base) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(base).join("rgit/config.toml"));
    }
    Ok(
        PathBuf::from(std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME is not set"))?)
            .join(".config/rgit/config.toml"),
    )
}

fn load_config() -> Result<ForgeConfigFile> {
    let path = config_path()?;
    let text = std::fs::read_to_string(&path).map_err(|error| {
        anyhow::anyhow!(
            "cannot read forge profile config {}: {error}",
            path.display()
        )
    })?;
    toml::from_str(&text).map_err(|error| {
        anyhow::anyhow!(
            "cannot parse forge profile config {}: {error}",
            path.display()
        )
    })
}

fn load_profile(name: &str) -> Result<ForgeProfile> {
    load_config()?
        .forge
        .profiles
        .get(name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("forge profile {name:?} is not configured"))
}
fn default_profile() -> Result<Option<String>> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(None);
    }
    Ok(load_config()?.forge.default_profile)
}

pub fn run(command: ForgeCmd, mut context: ForgeContext) -> Result<String> {
    if context.profile.is_none() {
        context.profile = default_profile()?;
    }
    if let Some(name) = context.profile.as_deref() {
        let profile = load_profile(name)?;
        if context.provider.is_none() {
            context.provider = profile.provider;
        }
        if context.account.is_none() {
            context.account = profile.account;
        }
        if context.host.is_none() {
            context.host = profile.host;
        }
    }
    if let Some(provider) = context.provider.as_deref() {
        ensure_provider(provider)?;
    }
    *FORGE_CONTEXT.lock().expect("forge context mutex") = context.clone();
    match command {
        ForgeCmd::Login {
            provider,
            host,
            account,
            token_stdin,
        } => {
            let host = host.unwrap_or_else(|| {
                if provider.eq_ignore_ascii_case("github") {
                    "https://github.com".to_owned()
                } else {
                    std::env::var("GITLAB_HOST").unwrap_or_else(|_| "https://gitlab.com".to_owned())
                }
            });
            if provider.eq_ignore_ascii_case("github") {
                if token_stdin {
                    let token = rgit_forge::read_token_stdin()?;
                    rgit_forge::save_credential_for(
                        "github",
                        &host,
                        &account,
                        &rgit_forge::StoredCredential {
                            access_token: token.to_string(),
                            refresh_token: None,
                            access_expires_at: None,
                            refresh_expires_at: None,
                        },
                    )?;
                    Ok(format!("stored GitHub credential for account {account:?}"))
                } else {
                    ensure!(
                        account == "default",
                        "browser login currently supports only the default account; use --token-stdin for named accounts"
                    );
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?
                        .block_on(rgit_forge::login_device(rgit_forge::DeviceFlowConfig {
                            provider: "github",
                            client_id: rgit_forge::GITHUB_OAUTH_CLIENT_ID,
                            authorize_endpoint: "https://github.com/login/oauth/authorize",
                            device_endpoint: "https://github.com/login/device/code",
                            token_endpoint: "https://github.com/login/oauth/access_token",
                            scope: "repo read:user",
                        }))?;
                    Ok("GitHub authentication completed".to_owned())
                }
            } else if provider.eq_ignore_ascii_case("gitlab") {
                ensure!(token_stdin, "GitLab login requires --token-stdin");
                let token = rgit_forge::read_token_stdin()?;
                rgit_forge::save_credential_for(
                    "gitlab",
                    &host,
                    &account,
                    &rgit_forge::StoredCredential {
                        access_token: token.to_string(),
                        refresh_token: None,
                        access_expires_at: None,
                        refresh_expires_at: None,
                    },
                )?;
                Ok(format!("stored GitLab credential for account {account:?}"))
            } else {
                anyhow::bail!("unsupported forge provider {provider:?}")
            }
        }
        ForgeCmd::Auth { cmd } => auth_command(cmd, forge_context()),
        ForgeCmd::Whoami {
            provider,
            host,
            account,
        } => match provider_name(provider.as_deref())? {
            provider if provider.eq_ignore_ascii_case("github") => with_github_account(&account),
            provider if provider.eq_ignore_ascii_case("gitlab") => {
                with_gitlab_account(host.as_deref(), &account)
            }
            provider => anyhow::bail!("unsupported forge provider {provider:?}"),
        },
        ForgeCmd::Logout {
            provider,
            host,
            account,
        } => {
            ensure_provider(&provider)?;
            let host = host.unwrap_or_else(|| {
                if provider.eq_ignore_ascii_case("github") {
                    "https://github.com".to_owned()
                } else {
                    "https://gitlab.com".to_owned()
                }
            });
            rgit_forge::delete_credential_for(&provider, &host, &account)?;
            Ok(format!(
                "removed {provider} credential for account {account:?}"
            ))
        }
        ForgeCmd::Repo { cmd } => {
            match cmd {
                RepoCmd::View { target, repo } => {
                    let (provider, repo) = resolve_target(target.as_deref(), repo.as_deref())?;
                    match provider.as_str() {
                        "github" => with_client(|client| async move {
                            print_json(client.repository(&repo).await?)
                        }),
                        "gitlab" => with_gitlab_client(|client| async move {
                            print_json(client.repository(&repo).await?)
                        }),
                        provider => anyhow::bail!("unsupported forge provider {provider:?}"),
                    }
                }
                RepoCmd::Create {
                    provider,
                    name,
                    organization,
                    description,
                    private,
                    auto_init,
                } => {
                    ensure_provider(&provider)?;
                    let request = CreateRepository {
                        name,
                        description,
                        private,
                        auto_init,
                    };
                    match provider.to_ascii_lowercase().as_str() {
                        "github" => with_client(|client| async move {
                            print_json(
                                client
                                    .create_repository(&request, organization.as_deref())
                                    .await?,
                            )
                        }),
                        "gitlab" => with_gitlab_client(|client| async move {
                            print_json(
                                client
                                    .create_repository(&request, organization.as_deref())
                                    .await?,
                            )
                        }),
                        provider => anyhow::bail!("unsupported forge provider {provider:?}"),
                    }
                }
                RepoCmd::Delete { target, repo, yes } => destructive(yes, "repository", || {
                    let (provider, repo) = resolve_target(target.as_deref(), repo.as_deref())?;
                    match provider.as_str() {
                        "github" => with_client(|client| async move {
                            client.delete_repository(&repo).await?;
                            Ok(format!("deleted github repository {}", repo.full_name()))
                        }),
                        "gitlab" => with_gitlab_client(|client| async move {
                            client.delete_repository(&repo).await?;
                            Ok(format!("deleted gitlab repository {}", repo.full_name()))
                        }),
                        provider => anyhow::bail!("unsupported forge provider {provider:?}"),
                    }
                }),
            }
        }
        ForgeCmd::Branch { cmd } => match cmd {
            ForgeBranchCmd::List { target, repo, page } => {
                let (provider, repo) = resolve_target(target.as_deref(), repo.as_deref())?;
                match provider.as_str() {
                    "github" => with_client(|client| async move {
                        print_json(client.branches(&repo, page).await?)
                    }),
                    "gitlab" => with_gitlab_client(|client| async move {
                        print_json(client.branches(&repo, page).await?)
                    }),
                    provider => anyhow::bail!("unsupported forge provider {provider:?}"),
                }
            }
            ForgeBranchCmd::Delete {
                target,
                repo,
                branch,
                yes,
            } => destructive(yes, "remote branch", || {
                let (provider, repo) = resolve_target(target.as_deref(), repo.as_deref())?;
                match provider.as_str() {
                    "github" => with_client(|client| async move {
                        client.delete_branch(&repo, &branch).await?;
                        Ok(format!("deleted {}/{}", repo.full_name(), branch))
                    }),
                    "gitlab" => with_gitlab_client(|client| async move {
                        client.delete_branch(&repo, &branch).await?;
                        Ok(format!("deleted {}/{}", repo.full_name(), branch))
                    }),
                    provider => anyhow::bail!("unsupported forge provider {provider:?}"),
                }
            }),
        },
        ForgeCmd::Pr { cmd } => match cmd {
            PrCmd::List { target, repo, page } => {
                let (provider, repo) = resolve_target(target.as_deref(), repo.as_deref())?;
                match provider.as_str() {
                    "github" => with_client(|client| async move {
                        print_json(client.pull_requests(&repo, page).await?)
                    }),
                    "gitlab" => with_gitlab_client(|client| async move {
                        print_json(client.merge_requests(&repo, page).await?)
                    }),
                    provider => anyhow::bail!("unsupported forge provider {provider:?}"),
                }
            }
            PrCmd::Create {
                target,
                repo,
                title,
                head,
                base,
                body,
                draft,
            } => {
                let (provider, repo) = resolve_target(target.as_deref(), repo.as_deref())?;
                let request = CreatePullRequest {
                    title,
                    head,
                    base,
                    body,
                    draft,
                };
                match provider.as_str() {
                    "github" => with_client(|client| async move {
                        print_json(client.create_pull_request(&repo, &request).await?)
                    }),
                    "gitlab" => with_gitlab_client(|client| async move {
                        print_json(client.create_merge_request(&repo, &request).await?)
                    }),
                    provider => anyhow::bail!("unsupported forge provider {provider:?}"),
                }
            }
            PrCmd::Close {
                target,
                repo,
                number,
                yes,
            } => destructive(yes, "pull request", || {
                let (provider, repo) = resolve_target(target.as_deref(), repo.as_deref())?;
                match provider.as_str() {
                    "github" => with_client(|client| async move {
                        print_json(client.close_pull_request(&repo, number).await?)
                    }),
                    "gitlab" => with_gitlab_client(|client| async move {
                        print_json(client.close_merge_request(&repo, number).await?)
                    }),
                    provider => anyhow::bail!("unsupported forge provider {provider:?}"),
                }
            }),
        },
    }
}
fn ensure_provider(provider: &str) -> Result<()> {
    if provider.eq_ignore_ascii_case("github") || provider.eq_ignore_ascii_case("gitlab") {
        Ok(())
    } else {
        anyhow::bail!("unsupported forge provider {provider:?}")
    }
}

fn provider_name(provider: Option<&str>) -> Result<&str> {
    let provider = provider.unwrap_or("github");
    ensure_provider(provider)?;
    Ok(provider)
}

fn auth_command(command: AuthCmd, context: ForgeContext) -> Result<String> {
    #[derive(Serialize)]
    struct AuthStatus {
        provider: String,
        host: String,
        account: String,
        authenticated: bool,
        source: &'static str,
    }

    fn provider_status(provider: &str, host: &str, env_present: bool) -> Result<Vec<AuthStatus>> {
        let identities = rgit_forge::credential_identities(provider)?;
        if identities.is_empty() {
            return Ok(vec![AuthStatus {
                provider: provider.to_owned(),
                host: host.to_owned(),
                account: "default".to_owned(),
                authenticated: env_present,
                source: if env_present { "environment" } else { "none" },
            }]);
        }
        Ok(identities
            .into_iter()
            .map(|identity| AuthStatus {
                provider: identity.provider,
                host: identity.host,
                account: identity.account,
                authenticated: true,
                source: "keychain",
            })
            .collect())
    }

    let status = match command {
        AuthCmd::Status => {
            let provider = context
                .provider
                .clone()
                .unwrap_or_else(|| "unknown".to_owned());
            let host = context
                .host
                .clone()
                .unwrap_or_else(|| "configured profile".to_owned());
            let account = context
                .account
                .clone()
                .unwrap_or_else(|| "default".to_owned());
            let env_present = match provider.as_str() {
                "github" => {
                    std::env::var("GITHUB_TOKEN").is_ok() || std::env::var("GH_TOKEN").is_ok()
                }
                "gitlab" => {
                    std::env::var("GITLAB_TOKEN").is_ok()
                        || std::env::var("GITLAB_ACCESS_TOKEN").is_ok()
                }
                _ => false,
            };
            vec![AuthStatus {
                provider,
                host,
                account,
                authenticated: env_present,
                source: if env_present {
                    "environment"
                } else {
                    "not_checked"
                },
            }]
        }
        AuthCmd::List => {
            let mut status = provider_status(
                "github",
                "https://github.com",
                std::env::var("GITHUB_TOKEN").is_ok() || std::env::var("GH_TOKEN").is_ok(),
            )?;
            status.extend(provider_status(
                "gitlab",
                &std::env::var("GITLAB_HOST").unwrap_or_else(|_| "https://gitlab.com".to_owned()),
                std::env::var("GITLAB_TOKEN").is_ok()
                    || std::env::var("GITLAB_ACCESS_TOKEN").is_ok(),
            )?);
            status
        }
    };
    Ok(print_json(serde_json::json!({
        "profile": context.profile,
        "accounts": status,
    }))?)
}

fn resolve_target(first: Option<&str>, second: Option<&str>) -> Result<(String, RepoRef)> {
    let (provider, repo) = match (first, second) {
        (Some(provider), Some(repo)) => (provider.to_owned(), repo.to_owned()),
        (Some(target), None) => match target.split_once(':') {
            Some((provider, repo)) => (provider.to_owned(), repo.to_owned()),
            None => ("github".to_owned(), target.to_owned()),
        },
        (None, Some(_)) => anyhow::bail!("a repository target is required before OWNER/REPO"),
        (None, None) => remote_target()?,
    };
    ensure_provider(&provider)?;
    Ok((provider.to_ascii_lowercase(), RepoRef::parse(&repo)?))
}

fn remote_target() -> Result<(String, String)> {
    use rgit_git::GitBackend;
    let remote = rgit_git::Git2Backend::discover(std::env::current_dir()?)
        .ok()
        .and_then(|backend| backend.config_get("remote.origin.url").ok().flatten());
    let remote =
        remote.ok_or_else(|| anyhow::anyhow!("no remote target; pass OWNER/REPO explicitly"))?;
    parse_remote_target(remote.trim())
}

fn parse_remote_target(remote: &str) -> Result<(String, String)> {
    let remote = remote.trim().trim_end_matches(".git");
    let (host, path) = if !remote.contains("://") {
        let (left, path) = remote
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("cannot parse git remote {remote:?}"))?;
        let host = left
            .rsplit_once('@')
            .map(|(_, host)| host)
            .ok_or_else(|| anyhow::anyhow!("cannot parse git remote {remote:?}"))?;
        (host, path)
    } else {
        let without_scheme = remote
            .split_once("://")
            .map(|(_, value)| value)
            .unwrap_or(remote);
        let (authority, path) = without_scheme
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("cannot parse git remote {remote:?}"))?;
        let host = authority
            .rsplit_once('@')
            .map(|(_, host)| host)
            .unwrap_or(authority);
        (host, path)
    };
    let path = path.trim_start_matches('/');
    let provider = if host.eq_ignore_ascii_case("github.com") {
        "github"
    } else if host.eq_ignore_ascii_case("gitlab.com")
        || std::env::var("GITLAB_HOST")
            .ok()
            .is_some_and(|configured| configured.trim_end_matches('/').contains(host))
    {
        "gitlab"
    } else {
        anyhow::bail!("unsupported forge remote host {host:?}")
    };
    Ok((provider.to_owned(), path.to_owned()))
}
fn with_github_account(account: &str) -> Result<String> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let client = GithubClient::from_environment_for(account).await?;
            print_json(client.current_user().await?)
        })?)
}
fn with_gitlab_account(host: Option<&str>, account: &str) -> Result<String> {
    let host = host.unwrap_or("https://gitlab.com").to_owned();
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let client = GitlabClient::from_environment_for(&host, account)?;
            print_json(client.current_user().await?)
        })?)
}

fn with_client<F, Fut>(operation: F) -> Result<String>
where
    F: FnOnce(GithubClient) -> Fut,
    Fut: Future<Output = Result<String, ForgeError>>,
{
    let context = forge_context();
    let account = context.account.unwrap_or_else(|| "default".to_owned());
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let client = GithubClient::from_environment_for(&account).await?;
            operation(client).await.map_err(Into::into)
        })
}

fn with_gitlab_client<F, Fut>(operation: F) -> Result<String>
where
    F: FnOnce(GitlabClient) -> Fut,
    Fut: Future<Output = Result<String, ForgeError>>,
{
    let context = forge_context();
    let account = context.account.unwrap_or_else(|| "default".to_owned());
    let host = context.host.unwrap_or_else(|| {
        std::env::var("GITLAB_HOST").unwrap_or_else(|_| "https://gitlab.com".to_owned())
    });
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let client = GitlabClient::from_environment_for(&host, &account)?;
            operation(client).await.map_err(Into::into)
        })
}

fn destructive<F>(yes: bool, object: &str, operation: F) -> Result<String>
where
    F: FnOnce() -> Result<String>,
{
    if !yes {
        return Err(anyhow::Error::new(crate::cli::CliError {
            message: format!("refusing to delete {object} without --yes"),
            help: Some("Re-run the same command with --yes to confirm".to_owned()),
            code: 2,
        }));
    }
    operation()
}
fn print_json<T: Serialize>(value: T) -> Result<String, ForgeError> {
    serde_json::to_string_pretty(&value).map_err(|error| ForgeError::Response(error.to_string()))
}

trait FullName {
    fn full_name(&self) -> String;
}

impl FullName for RepoRef {
    fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::parse_remote_target;

    #[test]
    fn parses_common_remote_urls() {
        assert_eq!(
            parse_remote_target("git@github.com:owner/repo.git").unwrap(),
            ("github".to_owned(), "owner/repo".to_owned())
        );
        assert_eq!(
            parse_remote_target("https://gitlab.com/group/subgroup/project.git").unwrap(),
            ("gitlab".to_owned(), "group/subgroup/project".to_owned())
        );
    }

    #[test]
    fn auth_status_uses_active_profile_without_keychain() {
        let output = super::auth_command(
            super::AuthCmd::Status,
            super::ForgeContext {
                profile: Some("work".to_owned()),
                provider: Some("gitlab".to_owned()),
                account: Some("work".to_owned()),
                host: Some("https://gitlab.example.com".to_owned()),
            },
        )
        .unwrap();
        assert!(output.contains("\"profile\": \"work\""));
        assert!(output.contains("\"account\": \"work\""));
        assert!(output.contains("\"source\": \"not_checked\""));
    }
}
