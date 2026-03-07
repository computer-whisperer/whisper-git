//! GitHub REST API client.
//!
//! Provides authenticated access to GitHub's API for fetching Actions build status,
//! creating repositories, and other GitHub-specific operations.

use anyhow::{Context, Result};
use serde::Deserialize;

const API_BASE: &str = "https://api.github.com";

pub struct GitHubClient {
    token: String,
}

/// Extract (owner, repo) from a GitHub remote URL.
/// Supports both HTTPS and SSH formats:
///   https://github.com/owner/repo.git
///   git@github.com:owner/repo.git
pub fn parse_github_remote(url: &str) -> Option<(String, String)> {
    let url = url.trim();

    // SSH: git@github.com:owner/repo.git
    if let Some(path) = url.strip_prefix("git@github.com:") {
        let path = path.strip_suffix(".git").unwrap_or(path);
        let mut parts = path.splitn(2, '/');
        let owner = parts.next()?;
        let repo = parts.next()?;
        if !owner.is_empty() && !repo.is_empty() {
            return Some((owner.to_string(), repo.to_string()));
        }
    }

    // HTTPS: https://github.com/owner/repo.git
    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.splitn(2, '/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    // Strip any trailing path segments (e.g. .git/info/...)
    let repo = repo.split('/').next()?;
    if !owner.is_empty() && !repo.is_empty() {
        Some((owner.to_string(), repo.to_string()))
    } else {
        None
    }
}

// --- Actions workflow run status ---

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowRun {
    pub id: u64,
    pub name: String,
    pub head_branch: String,
    pub head_sha: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub html_url: String,
}

#[derive(Debug, Deserialize)]
struct WorkflowRunsResponse {
    workflow_runs: Vec<WorkflowRun>,
}

// --- Repository creation ---

#[derive(Debug, Clone, Deserialize)]
pub struct CreatedRepo {
    pub full_name: String,
    pub clone_url: String,
    pub ssh_url: String,
}

impl GitHubClient {
    pub fn new(token: String) -> Self {
        Self { token }
    }

    fn get(&self, path: &str) -> Result<ureq::Response> {
        ureq::get(&format!("{API_BASE}{path}"))
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/vnd.github+json")
            .set("User-Agent", "whisper-git")
            .set("X-GitHub-Api-Version", "2022-11-28")
            .call()
            .context("GitHub API request failed")
    }

    fn post(&self, path: &str, body: &serde_json::Value) -> Result<ureq::Response> {
        ureq::post(&format!("{API_BASE}{path}"))
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/vnd.github+json")
            .set("User-Agent", "whisper-git")
            .set("X-GitHub-Api-Version", "2022-11-28")
            .send_json(body.clone())
            .context("GitHub API request failed")
    }

    /// Fetch the most recent workflow runs for a repo, optionally filtered by branch.
    pub fn workflow_runs(
        &self,
        owner: &str,
        repo: &str,
        branch: Option<&str>,
        per_page: u32,
    ) -> Result<Vec<WorkflowRun>> {
        let mut path = format!("/repos/{owner}/{repo}/actions/runs?per_page={per_page}");
        if let Some(branch) = branch {
            path.push_str(&format!("&branch={branch}"));
        }
        let resp = self.get(&path)?;
        let body: WorkflowRunsResponse = resp.into_json().context("Failed to parse runs")?;
        Ok(body.workflow_runs)
    }

    /// Create a new repository under the authenticated user's account.
    pub fn create_repo(&self, name: &str, private: bool) -> Result<CreatedRepo> {
        let body = serde_json::json!({
            "name": name,
            "private": private,
        });
        let resp = self.post("/user/repos", &body)?;
        let repo: CreatedRepo = resp.into_json().context("Failed to parse created repo")?;
        Ok(repo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ssh_url() {
        let (owner, repo) = parse_github_remote("git@github.com:user/project.git").unwrap();
        assert_eq!(owner, "user");
        assert_eq!(repo, "project");
    }

    #[test]
    fn parse_https_url() {
        let (owner, repo) =
            parse_github_remote("https://github.com/user/project.git").unwrap();
        assert_eq!(owner, "user");
        assert_eq!(repo, "project");
    }

    #[test]
    fn parse_https_no_dotgit() {
        let (owner, repo) = parse_github_remote("https://github.com/user/project").unwrap();
        assert_eq!(owner, "user");
        assert_eq!(repo, "project");
    }

    #[test]
    fn parse_non_github() {
        assert!(parse_github_remote("https://gitlab.com/user/project").is_none());
    }
}
