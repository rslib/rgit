use std::sync::{Arc, LazyLock, Mutex};

use crate::cli::{AuthCmd, ForgeBranchCmd, ForgeCmd, PrCmd, RepoCmd};
use anyhow::{Result, ensure};
use rgit_forge::{
    CreatePullRequest, CreateRepository, Forge, ForgeError, ForgeTarget, MergeMethod, Provider,
    StackEntry,
};
use rgit_git::GitBackend;
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
            PrCmd::View { what, target, repo } => {
                let target = resolve_target(&selection, target.as_deref(), repo.as_deref())?;
                let repo = target.repo()?.clone();
                let what = pr_what(what)?;
                with_forge(target, move |forge| async move {
                    let pr = match what {
                        PrWhat::Number(number) => forge.pull_request(&repo, number).await?,
                        PrWhat::Branch(branch) => forge
                            .pull_request_for_branch(&repo, &branch)
                            .await?
                            .ok_or_else(|| {
                                ForgeError::Response(format!(
                                    "no pull request for branch {branch:?}"
                                ))
                            })?,
                    };
                    print_json(pr)
                })
            }
            PrCmd::Checks {
                number,
                target,
                repo,
            } => {
                let target = resolve_target(&selection, target.as_deref(), repo.as_deref())?;
                let repo = target.repo()?.clone();
                let branch = if number.is_none() {
                    Some(current_branch()?)
                } else {
                    None
                };
                with_forge(target, move |forge| async move {
                    let number = match (number, branch) {
                        (Some(number), _) => number,
                        (None, Some(branch)) => {
                            forge
                                .pull_request_for_branch(&repo, &branch)
                                .await?
                                .ok_or_else(|| {
                                    ForgeError::Response(format!(
                                        "no pull request for branch {branch:?}"
                                    ))
                                })?
                                .number
                        }
                        (None, None) => unreachable!("a number or the current branch exists"),
                    };
                    print_json(forge.pull_request_checks(&repo, number).await?)
                })
            }
            PrCmd::List {
                target,
                repo,
                state,
                author,
                head,
                page,
            } => {
                let target = resolve_target(&selection, target.as_deref(), repo.as_deref())?;
                let repo = target.repo()?.clone();
                let state = state.into();
                with_forge(target, move |forge| async move {
                    print_json(
                        forge
                            .pull_requests(&repo, page, state, head.as_deref(), author.as_deref())
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
            PrCmd::Checkout {
                number,
                target,
                repo,
            } => {
                let target = resolve_target(&selection, target.as_deref(), repo.as_deref())?;
                let repo = target.repo()?.clone();
                let backend = rgit_git::Git2Backend::discover(std::env::current_dir()?)
                    .map_err(|error| anyhow::anyhow!("no repository: {error}"))?;
                with_forge(target, move |forge| async move {
                    checkout_pull_request(&forge, &repo, number, &backend).await
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
            PrCmd::Merge {
                target,
                repo,
                number,
                method,
                delete_branch,
                yes,
            } => destructive(yes, "pull request", || {
                let target = resolve_target(&selection, target.as_deref(), repo.as_deref())?;
                let repo = target.repo()?.clone();
                let method = match method.as_str() {
                    "squash" => MergeMethod::Squash,
                    "rebase" => MergeMethod::Rebase,
                    _ => MergeMethod::Merge,
                };
                with_forge(target, |forge| async move {
                    print_json(
                        forge
                            .merge_pull_request(&repo, number, method, delete_branch)
                            .await?,
                    )
                })
            }),
            PrCmd::Edit {
                target,
                repo,
                number,
                title,
                body,
                body_file,
                base,
            } => {
                if title.is_none() && body.is_none() && body_file.is_none() && base.is_none() {
                    return Err(crate::cli::CliError::usage(
                        "pr edit needs one of --title, --body, --body-file or --base",
                    ));
                }
                let target = resolve_target(&selection, target.as_deref(), repo.as_deref())?;
                let repo = target.repo()?.clone();
                let body = read_body(body, body_file)?;
                with_forge(target, move |forge| async move {
                    print_json(
                        forge
                            .edit_pull_request(
                                &repo,
                                number,
                                title.as_deref(),
                                body.as_deref(),
                                base.as_deref(),
                            )
                            .await?,
                    )
                })
            }
        },
        ForgeCmd::Browse {
            number,
            target,
            repo,
            print: show_url,
        } => {
            let target = resolve_target(&selection, target.as_deref(), repo.as_deref())?;
            let repo = target.repo()?.clone();
            with_forge(target, move |forge| async move {
                let url = match number {
                    Some(number) => forge.pull_request(&repo, number).await?.html_url,
                    None => forge.repository(&repo).await?.html_url,
                };
                if show_url {
                    Ok(url)
                } else {
                    open::that_detached(&url).map_err(|error| ForgeError::Api {
                        provider: forge.provider(),
                        message: error.to_string(),
                    })?;
                    Ok(format!("opened {url}"))
                }
            })
        }
    }
}

/// Ensure a pull request per pushed `(branch, base)` pair against the origin
/// remote's forge, each filled from its branch's tip commit. Returns one
/// `opened` or `updated` line per branch.
pub fn ensure_pull_requests(
    backend: &Arc<dyn GitBackend>,
    pairs: Vec<(String, String)>,
) -> Result<String> {
    let Some(origin) = backend.config_get("remote.origin.url")? else {
        anyhow::bail!("pushed; no pull requests: no origin remote is configured");
    };
    let selection = ForgeSelection::default().resolve()?;
    let target = ForgeTarget::from_remote(&selection, &origin)?;
    let repo = target.repo()?.clone();
    let mut entries = Vec::with_capacity(pairs.len());
    for (branch, base) in pairs {
        entries.push(StackEntry {
            message: backend.commit_message(&branch)?,
            branch,
            base,
        });
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let forge = Forge::connect(&target).await?;
        let lines = forge.ensure_pull_requests(&repo, &entries).await?;
        Ok(lines.join("\n"))
    })
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

/// What `pr checkout` does with a local branch for the fetched head: nothing
/// to change when the branch is at the head or absent, a fast-forward when the
/// branch is behind, and a refusal when it has moved elsewhere.
fn checkout_move(
    local_tip: Option<String>,
    fetched: &str,
    local_below: bool,
) -> Result<CheckoutAction> {
    match local_tip {
        None => Ok(CheckoutAction::Create),
        Some(tip) if tip == fetched => Ok(CheckoutAction::AtHead),
        Some(_) if local_below => Ok(CheckoutAction::FastForward),
        Some(_) => Err(anyhow::anyhow!(
            "local branch has diverged from the pull request's head {fetched}; delete, reset or rename it first"
        )),
    }
}

enum CheckoutAction {
    AtHead,
    FastForward,
    Create,
}

async fn checkout_pull_request(
    forge: &Forge,
    repo: &rgit_forge::RepoRef,
    number: u64,
    backend: &dyn GitBackend,
) -> Result<String, ForgeError> {
    use rgit_git::FetchArgs;
    let pr = forge.pull_request(repo, number).await?;
    let branch = pr.head_branch.clone();
    let fetched_ref = format!("refs/rgit/pr/{number}");
    let report = |_: rgit_git::OpProgress| {};
    backend
        .fetch(
            Some("origin"),
            &[format!("+{}:{fetched_ref}", forge.pull_request_ref(number))],
            &FetchArgs::default(),
            &report,
        )
        .map_err(|_| {
            ForgeError::Response(format!(
                "cannot fetch {repo} pull request {number} from origin"
            ))
        })?;
    let pulled = backend
        .rev_parse(&fetched_ref)
        .map_err(|error| ForgeError::Response(error.to_string()))?;
    let local_tip = backend
        .branch_tip(&branch)
        .map_err(|error| ForgeError::Response(error.to_string()))?;
    let local_below = match &local_tip {
        Some(tip) => backend
            .merge_bases(tip, &pulled, true)
            .map_err(|error| ForgeError::Response(error.to_string()))?
            .iter()
            .any(|base| base.eq_ignore_ascii_case(tip)),
        None => false,
    };
    let action = checkout_move(local_tip, &pulled, local_below)
        .map_err(|error| ForgeError::Response(error.to_string()))?;
    let on_branch = backend
        .status()
        .ok()
        .and_then(|status| status.head.branch)
        .is_some_and(|current| current == branch);
    match action {
        CheckoutAction::AtHead => (),
        CheckoutAction::FastForward => {
            backend
                .update_ref(
                    &format!("refs/heads/{branch}"),
                    Some(&pulled),
                    None,
                    false,
                    Some(&format!("checkout: PR #{number}")),
                )
                .map_err(|error| ForgeError::Response(error.to_string()))?;
        }
        CheckoutAction::Create => {
            backend
                .branch_from(&branch, &pulled, false, false)
                .map_err(|error| ForgeError::Response(error.to_string()))?;
        }
    }
    if !on_branch {
        backend
            .checkout_branch(&branch)
            .map_err(|error| ForgeError::Response(error.to_string()))?;
    }
    Ok(format!("checked out {branch} (PR #{number})"))
}

/// The pull request a `pr view` or `pr checks` argument names: a number when
/// the argument is numeric, else a head branch.
enum PrWhat {
    Number(u64),
    Branch(String),
}

fn pr_what(what: Option<String>) -> Result<PrWhat> {
    match what {
        Some(value) if value.chars().all(char::is_numeric) && !value.is_empty() => {
            Ok(PrWhat::Number(value.parse()?))
        }
        Some(value) => Ok(PrWhat::Branch(value)),
        None => Ok(PrWhat::Branch(current_branch()?)),
    }
}

fn current_branch() -> Result<String> {
    let backend = rgit_git::Git2Backend::discover(std::env::current_dir()?)
        .map_err(|_| anyhow::anyhow!("no repository; pass a PR number or branch"))?;
    backend
        .status()
        .ok()
        .and_then(|status| status.head.branch)
        .ok_or_else(|| anyhow::anyhow!("HEAD is detached; pass a PR number or branch"))
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

/// The body a `--body` flag or a `--body-file` file carries. `-` reads stdin.
fn read_body(body: Option<String>, body_file: Option<String>) -> Result<Option<String>> {
    match (body, body_file) {
        (Some(_), Some(_)) => Err(crate::cli::CliError::usage(
            "--body and --body-file cannot be given together",
        )),
        (body, None) => Ok(body),
        (None, Some(path)) => {
            let text = if path == "-" {
                use std::io::Read;
                let mut text = String::new();
                std::io::stdin().read_to_string(&mut text)?;
                text
            } else {
                std::fs::read_to_string(&path)?
            };
            Ok(Some(text))
        }
    }
}

fn print_json<T: Serialize>(value: T) -> Result<String, ForgeError> {
    serde_json::to_string_pretty(&value).map_err(|error| ForgeError::Response(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{AuthCmd, CheckoutAction, ForgeSelection, checkout_move};

    #[test]
    fn checkout_moves_by_local_tip() {
        let fetched = "abc123";
        assert!(matches!(
            checkout_move(None, fetched, false).expect("move"),
            CheckoutAction::Create
        ));
        assert!(matches!(
            checkout_move(Some(fetched.to_owned()), fetched, false).expect("move"),
            CheckoutAction::AtHead
        ));
        assert!(matches!(
            checkout_move(Some("old".to_owned()), fetched, true).expect("move"),
            CheckoutAction::FastForward
        ));
        assert!(checkout_move(Some("old".to_owned()), fetched, false).is_err());
    }

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
