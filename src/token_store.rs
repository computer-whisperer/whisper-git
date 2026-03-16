//! Secure credential storage for API tokens.
//!
//! Uses the system keychain (Secret Service on Linux, Keychain on macOS,
//! Credential Manager on Windows) via the `keyring` crate. Falls back to
//! plaintext config storage if the keychain is unavailable.

use std::collections::HashMap;

const SERVICE: &str = "whisper-git";
const GITHUB_USER: &str = "github-token";

/// Prefix for GitLab token entries in the keychain.
/// Full username is "gitlab-token:{hostname}" e.g. "gitlab-token:gitlab.com".
const GITLAB_PREFIX: &str = "gitlab-token:";

/// Try to get a keychain entry. Returns None if keychain is unavailable.
fn entry(username: &str) -> Option<keyring::Entry> {
    keyring::Entry::new(SERVICE, username).ok()
}

/// Read a token from the system keychain.
pub fn get_github_token() -> Option<String> {
    entry(GITHUB_USER)
        .and_then(|e| e.get_password().ok())
        .filter(|s| !s.is_empty())
}

/// Store a token in the system keychain.
/// Returns true on success, false if keychain is unavailable.
pub fn set_github_token(token: &str) -> bool {
    entry(GITHUB_USER).is_some_and(|e| e.set_password(token).is_ok())
}

/// Delete the GitHub token from the keychain.
pub fn delete_github_token() -> bool {
    entry(GITHUB_USER).is_some_and(|e| e.delete_credential().is_ok())
}

/// Read a GitLab token for a specific host from the keychain.
pub fn get_gitlab_token(host: &str) -> Option<String> {
    let username = format!("{GITLAB_PREFIX}{host}");
    entry(&username)
        .and_then(|e| e.get_password().ok())
        .filter(|s| !s.is_empty())
}

/// Store a GitLab token for a specific host in the keychain.
pub fn set_gitlab_token(host: &str, token: &str) -> bool {
    let username = format!("{GITLAB_PREFIX}{host}");
    entry(&username).is_some_and(|e| e.set_password(token).is_ok())
}

/// Delete a GitLab token for a specific host from the keychain.
pub fn delete_gitlab_token(host: &str) -> bool {
    let username = format!("{GITLAB_PREFIX}{host}");
    entry(&username).is_some_and(|e| e.delete_credential().is_ok())
}

/// Whether the system keychain is available at all.
pub fn is_available() -> bool {
    entry("probe").is_some_and(|e| {
        // Try a harmless operation to verify the backend works
        match e.get_password() {
            Ok(_) => true,
            Err(keyring::Error::NoEntry) => true, // backend works, just no entry
            Err(_) => false,                      // backend broken
        }
    })
}

/// Migrate plaintext tokens from Config into the keychain.
/// Returns (migrated_count, Vec<error_messages>).
/// On success, the plaintext fields should be cleared by the caller.
pub fn migrate_from_config(
    github_token: Option<&str>,
    gitlab_tokens: &HashMap<String, String>,
) -> (u32, Vec<String>) {
    let mut migrated = 0u32;
    let mut errors = Vec::new();

    if let Some(token) = github_token.filter(|t| !t.is_empty()) {
        if set_github_token(token) {
            migrated += 1;
        } else {
            errors.push("Failed to migrate GitHub token to keychain".into());
        }
    }

    for (host, token) in gitlab_tokens {
        if !token.is_empty() {
            if set_gitlab_token(host, token) {
                migrated += 1;
            } else {
                errors.push(format!(
                    "Failed to migrate GitLab token for {host} to keychain"
                ));
            }
        }
    }

    (migrated, errors)
}
