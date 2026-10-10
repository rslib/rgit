use std::sync::{LazyLock, Mutex};

use crate::cli::{AuthCmd, ForgeBranchCmd, ForgeCmd, PrCmd, RepoCmd};
use anyhow::{Result, ensure};
use rgit_forge::{
    CreatePullRequest, CreateRepository, Forge, ForgeError, ForgeTarget, Provider, PullRequestState,
};
use serde::Serialize;

pub use rgit_forge::ForgeSelection;

static FORGE_SELECTION: LazyLock<Mutex<ForgeSelection>> =
    LazyLock::new(|| Mutex::new(ForgeSelection::default()));

fn active() -> ForgeSelection {
    FORGE_SELECTION
        .lock()
        .expect("forge selection mutex")
        .clone()
}

pub fn run(command: ForgeCmd, selection: ForgeSelection) -> Result<String> {
    let selection = selection.resolve()?;
    if let Some(provider) = selection.provider.as_deref() {
        Provider::parse(provider)?;
    }
    *FORGE_SELECTION.lock().expect("forge selection mutex") = selection.clone();
    match command {
        ForgeCmd::Login {
            provider,
            host,
            account,
            token_stdin,
        } => {
            let provider = Provider::parse(&provider)?;
            let host = host.unwrap_or_else(|| {
                if provider == Provider::Github {
                    "https://github.com".to_owned()
                } else {
                    std::env::var("GITLAB_HOST").unwrap_or_else(|_| "https://gitlab.com".to_owned())
                }
            });
            if provider == Provider::Github {
                if token_stdin {
                    let token = rgit_forge::read_token_stdin()?;
                    rgit_forge::save_credential_for(
                        provider.as_str(),
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
                            provider: provider.as_str(),
                            client_id: rgit_forge::GITHUB_OAUTH_CLIENT_ID,
                            authorize_endpoint: "https://github.com/login/oauth/authorize",
                            device_endpoint: "https://github.com/login/device/code",
                            token_endpoint: "https://github.com/login/oauth/access_token",
                            scope: "repo read:user",
                        }))?;
                    Ok("GitHub authentication completed".to_owned())
                }
            } else {
                ensure!(token_stdin, "GitLab login requires --token-stdin");
                let token = rgit_forge::read_token_stdin()?;
                rgit_forge::save_credential_for(
                    provider.as_str(),
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
            }
        }
        ForgeCmd::Auth { cmd } => auth_command(cmd, active()),
        ForgeCmd::Whoami {
            provider,
            host,
            account,
        } => {
            let provider = provider
                .as_deref()
                .map(Provider::parse)
                .transpose()?
                .unwrap_or(Provider::Github);
            let target = ForgeTarget {
                provider,
                host: host.unwrap_or_else(|| provider.default_host().to_owned()),
                account,
                repo: None,
            };
            with_forge(target, |forge| async move {
                print_json(forge.current_user().await?)
            })
        }
        ForgeCmd::Logout {
            provider,
            host,
            account,
        } => {
            let provider = Provider::parse(&provider)?;
            let host = host.unwrap_or_else(|| provider.default_host().to_owned());
            rgit_forge::delete_credential_for(provider.as_str(), &host, &account)?;
            Ok(format!(
                "removed {provider} credential for account {account:?}"
            ))
        }
        ForgeCmd::Repo { cmd } => match cmd {
            RepoCmd::View { target, repo } => {
                let target = resolve_target(&selection, target.as_deref(), repo.as_deref())?;
                let repo = target.repo()?.clone();
                with_forge(target, |forge| async move {
                    print_json(forge.repository(&repo).await?)
                })
            }
            RepoCmd::Create {
                provider,
                name,
                organization,
                description,
                private,
                auto_init,
            } => {
                let target = ForgeTarget::for_provider(&selection, Provider::parse(&provider)?);
                let request = CreateRepository {
                    name,
                    description,
                    private,
                    auto_init,
                };
                with_forge(target, |forge| async move {
                    print_json(
                        forge
                            .create_repository(&request, organization.as_deref())
                            .await?,
                    )
                })
            }
            RepoCmd::Delete { target, repo, yes } => destructive(yes, "repository", || {
                let target = resolve_target(&selection, target.as_deref(), repo.as_deref())?;
                let repo = target.repo()?.clone();
                let provider = target.provider;
                with_forge(target, async move |forge| {
                    forge.delete_repository(&repo).await?;
                    Ok(format!("deleted {provider} repository {repo}"))
                })
            }),
        },
        ForgeCmd::Branch { cmd } => match cmd {
            ForgeBranchCmd::List { target, repo, page } => {
                let target = resolve_target(&selection, target.as_deref(), repo.as_deref())?;
                let repo = target.repo()?.clone();
                with_forge(target, |forge| async move {
                    print_json(forge.branches(&repo, page).await?)
                })
            }
            ForgeBranchCmd::Delete {
                target,
                repo,
                branch,
                yes,
            } => destructive(yes, "remote branch", || {
                let target = resolve_target(&selection, target.as_deref(), repo.as_deref())?;
                let repo = target.repo()?.clone();
                with_forge(target, async move |forge| {
                    forge.delete_branch(&repo, &branch).await?;
                    Ok(format!("deleted {repo}/{branch}"))
                })
            }),
        },
        ForgeCmd::Pr { cmd } => match cmd {
            PrCmd::List { target, repo, page } => {
                let target = resolve_target(&selection, target.as_deref(), repo.as_deref())?;
                let repo = target.repo()?.clone();
                with_forge(target, |forge| async move {
                    print_json(
                        forge
                            .pull_requests(&repo, page, PullRequestState::Open, None)
                            .await?,
                    )
                })
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
                let target = resolve_target(&selection, target.as_deref(), repo.as_deref())?;
                let request = CreatePullRequest {
                    title,
                    head,
                    base,
                    body,
                    draft,
                };
                let repo = target.repo()?.clone();
                with_forge(target, |forge| async move {
                    print_json(forge.create_pull_request(&repo, &request).await?)
                })
            }
            PrCmd::Close {
                target,
                repo,
                number,
                yes,
            } => destructive(yes, "pull request", || {
                let target = resolve_target(&selection, target.as_deref(), repo.as_deref())?;
                let repo = target.repo()?.clone();
                with_forge(target, |forge| async move {
                    print_json(forge.close_pull_request(&repo, number).await?)
                })
            }),
        },
    }
}

/// The repository an operation acts on: the arguments, or the origin remote
/// when neither names one.
fn resolve_target(
    selection: &ForgeSelection,
    target: Option<&str>,
    repo: Option<&str>,
) -> Result<ForgeTarget> {
    if target.is_none() && repo.is_none() {
        let remote = origin_remote()?;
        return Ok(ForgeTarget::from_remote(selection, &remote)?);
    }
    Ok(ForgeTarget::from_argument(selection, target, repo)?)
}

fn origin_remote() -> Result<String> {
    use rgit_git::GitBackend;
    rgit_git::Git2Backend::discover(std::env::current_dir()?)
        .ok()
        .and_then(|backend| backend.config_get("remote.origin.url").ok().flatten())
        .ok_or_else(|| anyhow::anyhow!("no remote target; pass OWNER/REPO explicitly"))
}

fn auth_command(command: AuthCmd, selection: ForgeSelection) -> Result<String> {
    #[derive(Serialize)]
    struct AuthStatus {
        provider: String,
        host: String,
        account: String,
        authenticated: bool,
        source: &'static str,
    }

    fn provider_status(provider: Provider) -> Result<Vec<AuthStatus>, ForgeError> {
        let identities = rgit_forge::credential_identities(provider.as_str())?;
        let authenticated = provider.env_token().is_some();
        if identities.is_empty() {
            return Ok(vec![AuthStatus {
                provider: provider.as_str().to_owned(),
                host: provider
                    .env_host()
                    .unwrap_or_else(|| provider.default_host().to_owned()),
                account: "default".to_owned(),
                authenticated,
                source: if authenticated { "environment" } else { "none" },
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
            let provider = selection
                .provider
                .clone()
                .unwrap_or_else(|| "unknown".to_owned());
            let authenticated = Provider::parse(&provider)
                .ok()
                .and_then(|provider| provider.env_token())
                .is_some();
            vec![AuthStatus {
                provider,
                host: selection
                    .host
                    .clone()
                    .unwrap_or_else(|| "configured profile".to_owned()),
                account: selection.account.unwrap_or_else(|| "default".to_owned()),
                authenticated,
                source: if authenticated {
                    "environment"
                } else {
                    "not_checked"
                },
            }]
        }
        AuthCmd::List => Provider::ALL
            .into_iter()
            .map(provider_status)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect(),
    };
    Ok(print_json(serde_json::json!({
        "profile": selection.profile,
        "accounts": status,
    }))?)
}

fn with_forge<F, Fut>(target: ForgeTarget, operation: F) -> Result<String>
where
    F: FnOnce(Forge) -> Fut,
    Fut: Future<Output = Result<String, ForgeError>>,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let forge = Forge::connect(&target).await?;
        operation(forge).await.map_err(Into::into)
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

#[cfg(test)]
mod tests {
    use super::{AuthCmd, ForgeSelection};

    #[test]
    fn parses_common_remote_urls() {
        let selection = ForgeSelection::default();
        let target =
            rgit_forge::ForgeTarget::from_remote(&selection, "git@github.com:owner/repo.git")
                .expect("github remote");
        assert_eq!(target.provider, rgit_forge::Provider::Github);
        assert_eq!(target.repo().expect("repo").to_string(), "owner/repo");

        let target = rgit_forge::ForgeTarget::from_remote(
            &selection,
            "https://gitlab.com/group/subgroup/project.git",
        )
        .expect("gitlab remote");
        assert_eq!(target.provider, rgit_forge::Provider::Gitlab);
        assert_eq!(
            target.repo().expect("repo").to_string(),
            "group/subgroup/project"
        );
    }

    #[test]
    fn auth_status_uses_active_profile_without_keychain() {
        let output = super::auth_command(
            AuthCmd::Status,
            ForgeSelection {
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
