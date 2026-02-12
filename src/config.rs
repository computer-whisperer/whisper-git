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
}

fn default_true() -> bool { true }
fn default_one() -> f32 { 1.0 }

/// Maximum number of recent repos to remember
const MAX_RECENT_REPOS: usize = 10;

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
        }
    }
}

impl Config {
    fn config_dir() -> Option<PathBuf> {
        std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".config").join("whisper-git"))
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
        serde_json::from_str(&data).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(dir) = Self::config_dir() else {
            return;
        };
        if let Err(e) = fs::create_dir_all(&dir) {
            eprintln!("Failed to create config dir: {e}");
            return;
        }
        let Some(path) = Self::config_path() else {
            return;
        };
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = fs::write(&path, json) {
                    eprintln!("Failed to save config: {e}");
                }
            }
            Err(e) => eprintln!("Failed to serialize config: {e}"),
        }
    }

    /// Add a repo path to the recent repos list (most recent first, deduped).
    pub fn add_recent_repo(&mut self, path: &str) {
        self.recent_repos.retain(|p| p != path);
        self.recent_repos.insert(0, path.to_string());
        self.recent_repos.truncate(MAX_RECENT_REPOS);
        self.save();
    }
}
