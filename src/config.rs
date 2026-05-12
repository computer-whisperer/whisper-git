//! User settings persistence to ~/.config/whisper-git/settings.json.
//!
//! Manages application settings (avatars, scroll speed, row scale, etc.) via serde_json serialization.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub avatars_enabled: bool,
    pub fast_scroll: bool,
    pub row_scale: f32,
    pub shortcut_bar_visible: bool,
    #[serde(default)]
    pub recent_repos: Vec<String>,
    #[serde(default = "default_true")]
    pub abbreviate_worktree_names: bool,
    #[serde(default = "default_one")]
    pub time_spacing_strength: f32,
    #[serde(default = "default_true")]
    pub show_orphaned_commits: bool,
    #[serde(default = "default_true")]
    pub ratchet_scroll: bool,
    #[serde(default = "default_ai_provider")]
    pub ai_provider: String,
    /// Registered GitLab hosts (e.g. "gitlab.com", "gitlab.company.com").
    /// Just the host registry — the token modal enumerates these and
    /// looks up actual secrets from the keychain via `token_store`.
    /// Auto-populated when a GitLab remote is detected during CI fetch.
    #[serde(default)]
    pub gitlab_hosts: Vec<String>,
    /// User-resized left sidebar width (logical px). Persisted across
    /// restarts via this config so the layout the user worked into
    /// survives a relaunch.
    #[serde(default = "default_sidebar_w")]
    pub sidebar_w: f32,
    /// User-resized right pane width — staging well in Working view,
    /// commit details pane in History view.
    #[serde(default = "default_right_w")]
    pub right_pane_w: f32,
    /// `true` for side-by-side diff view; `false` for unified.
    #[serde(default)]
    pub diff_split: bool,
}

fn default_sidebar_w() -> f32 {
    aetna_core::tokens::SIDEBAR_WIDTH
}
fn default_right_w() -> f32 {
    420.0
}

fn default_true() -> bool {
    true
}
fn default_one() -> f32 {
    1.0
}
fn default_ai_provider() -> String {
    "claude-cli".to_string()
}

/// Maximum number of recent repos to remember
pub(crate) const MAX_RECENT_REPOS: usize = 10;

impl Default for Config {
    fn default() -> Self {
        Self {
            avatars_enabled: true,
            fast_scroll: false,
            row_scale: 1.0,
            shortcut_bar_visible: true,
            recent_repos: Vec::new(),
            abbreviate_worktree_names: true,
            time_spacing_strength: 1.0,
            show_orphaned_commits: true,
            ratchet_scroll: true,
            ai_provider: default_ai_provider(),
            gitlab_hosts: Vec::new(),
            sidebar_w: default_sidebar_w(),
            right_pane_w: default_right_w(),
            diff_split: false,
        }
    }
}

impl Config {
    fn config_dir() -> Option<PathBuf> {
        std::env::var("HOME")
            .ok()
            .map(|h| PathBuf::from(h).join(".config").join("whisper-git"))
    }

    fn config_path() -> Option<PathBuf> {
        Self::config_dir().map(|d| d.join("settings.json"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };
        let Ok(data) = fs::read_to_string(&path) else {
            return Self::default();
        };
        let mut config: Self = serde_json::from_str(&data).unwrap_or_default();
        if let Err(e) = config.refresh_recent_repos() {
            eprintln!("Warning: failed to refresh recent repositories: {e}");
        }
        config
    }

    pub fn save(&self) -> Result<(), String> {
        let dir =
            Self::config_dir().ok_or_else(|| "Could not determine config directory".to_string())?;
        fs::create_dir_all(&dir).map_err(|e| format!("Failed to create config dir: {e}"))?;
        let path =
            Self::config_path().ok_or_else(|| "Could not determine config path".to_string())?;
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize config: {e}"))?;
        fs::write(&path, json).map_err(|e| format!("Failed to save config: {e}"))?;
        Ok(())
    }

    /// Register `host` (e.g. "gitlab.com") if not already known. Returns
    /// `true` when the registry changed so the caller can persist.
    /// Hosts are kept sorted to give the modal a stable display order.
    pub fn register_gitlab_host(&mut self, host: &str) -> bool {
        if self.gitlab_hosts.iter().any(|h| h == host) {
            return false;
        }
        self.gitlab_hosts.push(host.to_string());
        self.gitlab_hosts.sort();
        true
    }

    /// Add a repo path to the recent repos list (most recent first, deduped).
    pub fn add_recent_repo(&mut self, path: &str) -> Result<(), String> {
        self.recent_repos.retain(|p| p != path);
        self.recent_repos.insert(0, path.to_string());
        self.recent_repos = crate::recent::compact_recent_paths(&self.recent_repos);
        self.recent_repos.truncate(MAX_RECENT_REPOS);
        self.save()
    }

    /// Drop stale recent entries, canonicalize worktree paths to repo
    /// paths, and dedupe multiple references to the same shared repo.
    pub fn refresh_recent_repos(&mut self) -> Result<(), String> {
        let mut compacted = crate::recent::compact_recent_paths(&self.recent_repos);
        compacted.truncate(MAX_RECENT_REPOS);
        if compacted == self.recent_repos {
            return Ok(());
        }
        self.recent_repos = compacted;
        self.save()
    }
}
