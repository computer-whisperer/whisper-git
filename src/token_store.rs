//! Secure credential storage for API tokens.
//!
//! Uses the system keychain (Secret Service on Linux, Keychain on macOS,
//! Credential Manager on Windows) via the `keyring` crate.
//!
//! Reads are cached in-process: the first lookup hits the keychain, subsequent
//! lookups return the cached value. The cache is updated by our own
//! set/delete functions, so state stays consistent within a single app run.
//! External edits (e.g. via Seahorse) only take effect after a restart.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

const SERVICE: &str = "whisper-git";
const GITHUB_USER: &str = "github-token";

/// Prefix for GitLab token entries in the keychain.
/// Full username is "gitlab-token:{hostname}" e.g. "gitlab-token:gitlab.com".
const GITLAB_PREFIX: &str = "gitlab-token:";

/// Cached token lookups keyed by keychain username. A present entry with
/// value `None` means "we've checked and there is no usable token" — we still
/// return `None` without touching the keychain again.
static TOKEN_CACHE: LazyLock<Mutex<HashMap<String, Option<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Cached result of the backend availability probe.
static BACKEND_AVAILABLE: LazyLock<Mutex<Option<bool>>> = LazyLock::new(|| Mutex::new(None));

/// Try to get a keychain entry. Returns None if keychain is unavailable.
fn entry(username: &str) -> Option<keyring::Entry> {
    keyring::Entry::new(SERVICE, username).ok()
}

/// Read a token, using the process-wide cache. The keychain is only hit on
/// the first call for a given `username`; subsequent calls return the cached
/// result even if it was `None`, so we don't pop repeated unlock prompts on
/// every poll tick.
fn cached_get(username: &str) -> Option<String> {
    if let Ok(cache) = TOKEN_CACHE.lock()
        && let Some(cached) = cache.get(username)
    {
        return cached.clone();
    }

    let value = entry(username)
        .and_then(|e| e.get_password().ok())
        .filter(|s| !s.is_empty());

    if let Ok(mut cache) = TOKEN_CACHE.lock() {
        cache.insert(username.to_string(), value.clone());
    }
    value
}

fn cache_store(username: &str, value: Option<String>) {
    if let Ok(mut cache) = TOKEN_CACHE.lock() {
        cache.insert(username.to_string(), value);
    }
}

/// Write `token` to the keychain and update the cache. Returns false if the
/// keychain is unavailable or the write failed.
fn cached_set(username: &str, token: &str) -> bool {
    let Some(e) = entry(username) else {
        return false;
    };
    let ok = e.set_password(token).is_ok();
    cache_store(
        username,
        (ok && !token.is_empty()).then(|| token.to_string()),
    );
    ok
}

/// Delete the keychain entry and clear it from the cache.
fn cached_delete(username: &str) -> bool {
    let Some(e) = entry(username) else {
        return false;
    };
    let ok = e.delete_credential().is_ok();
    cache_store(username, None);
    ok
}

/// Read a token from the system keychain.
pub fn get_github_token() -> Option<String> {
    cached_get(GITHUB_USER)
}

/// Store a token in the system keychain.
/// Returns true on success, false if keychain is unavailable.
pub fn set_github_token(token: &str) -> bool {
    cached_set(GITHUB_USER, token)
}

/// Delete the GitHub token from the keychain.
pub fn delete_github_token() -> bool {
    cached_delete(GITHUB_USER)
}

/// Read a GitLab token for a specific host from the keychain.
pub fn get_gitlab_token(host: &str) -> Option<String> {
    cached_get(&format!("{GITLAB_PREFIX}{host}"))
}

/// Store a GitLab token for a specific host in the keychain.
pub fn set_gitlab_token(host: &str, token: &str) -> bool {
    cached_set(&format!("{GITLAB_PREFIX}{host}"), token)
}

/// Delete a GitLab token for a specific host from the keychain.
pub fn delete_gitlab_token(host: &str) -> bool {
    cached_delete(&format!("{GITLAB_PREFIX}{host}"))
}

/// Whether the system keychain is available at all. Cached after first probe.
pub fn is_available() -> bool {
    let Ok(mut cached) = BACKEND_AVAILABLE.lock() else {
        return false;
    };
    *cached.get_or_insert_with(|| {
        entry("probe").is_some_and(|e| match e.get_password() {
            Ok(_) => true,
            Err(keyring::Error::NoEntry) => true, // backend works, just no entry
            Err(_) => false,                      // backend broken
        })
    })
}
