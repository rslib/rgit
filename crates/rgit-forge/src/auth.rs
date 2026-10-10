//! Credential storage in the OS keychain and the OAuth login flows.

use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use base64::Engine;
use keyring::Entry;
use rand::RngExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::ForgeError;
use super::types::{CredentialIdentity, StoredCredential};

const KEYRING_SERVICE: &str = "rgit.forge";
pub const GITHUB_OAUTH_CLIENT_ID: &str = "Ov23lizdJOnx6whPL9rw";

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

pub(crate) async fn refresh_if_needed(
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
    Ok(Some(StoredCredential::from_json_or_token(value)))
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
    Ok(Some(StoredCredential::from_json_or_token(value)))
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
    fn a_bare_token_reads_as_an_access_token() {
        let credential = StoredCredential::from_json_or_token("token".to_owned());
        assert_eq!(credential.access_token, "token");
        assert_eq!(credential.refresh_token, None);
    }
}
