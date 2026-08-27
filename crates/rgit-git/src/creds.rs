//! Interactive credential prompting for network operations. The backend first
//! tries non-interactive sources (ssh-agent, `~/.ssh` keys, git credential
//! helpers); only when those cannot authenticate does it ask a `CredentialPrompt`
//! for a secret. A frontend that cannot prompt (e.g. a headless server) installs
//! none, so the operation fails cleanly with guidance as before.

/// Asks the user for a credential the non-interactive sources could not supply.
/// Implementations read from a terminal (hidden input for secrets). Returning
/// `None` cancels, and the operation fails.
pub trait CredentialPrompt: Send + Sync {
    /// A username for `url` (echoed). Asked only when the remote did not supply
    /// one (typically an HTTPS remote without a user in the URL).
    fn username(&self, url: &str) -> Option<String>;

    /// A password or personal-access token for `user` at `url` (hidden input).
    fn password(&self, url: &str, user: &str) -> Option<String>;

    /// A passphrase to unlock an SSH private key (hidden input).
    fn ssh_passphrase(&self, key: &str) -> Option<String>;
}
