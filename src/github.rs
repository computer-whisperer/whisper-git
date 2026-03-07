//! GitHub REST API client.
//!
//! Provides authenticated access to GitHub's API for fetching Actions build status,
//! creating repositories, and other GitHub-specific operations.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::sync::mpsc::{self, Receiver};
use winit::event_loop::EventLoopProxy;

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
        let (owner, repo) = path.split_once('/')?;
        if !owner.is_empty() && !repo.is_empty() {
            return Some((owner.to_string(), repo.to_string()));
        }
    }

    // HTTPS: https://github.com/owner/repo.git
    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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

    #[allow(dead_code)]
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
    #[allow(dead_code)]
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

// --- Repository listing ---

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct RepoInfo {
    pub full_name: String,
    pub clone_url: String,
    pub ssh_url: String,
    pub private: bool,
    pub description: Option<String>,
    #[serde(default)]
    pub fork: bool,
    pub updated_at: Option<String>,
}

impl GitHubClient {
    /// List repositories accessible to the authenticated user, sorted by most recently pushed.
    /// Fetches up to `max_pages` pages of 100 repos each.
    pub fn list_repos(&self, max_pages: u32) -> Result<Vec<RepoInfo>> {
        let mut all = Vec::new();
        for page in 1..=max_pages {
            let path = format!("/user/repos?sort=pushed&direction=desc&per_page=100&page={page}");
            let resp = self.get(&path)?;
            let repos: Vec<RepoInfo> = resp.into_json().context("Failed to parse repo list")?;
            let done = repos.len() < 100;
            all.extend(repos);
            if done {
                break;
            }
        }
        Ok(all)
    }
}

/// Fetch the authenticated user's repo list asynchronously.
/// Returns None if no token is provided.
pub fn fetch_repo_list_async(
    token: &str,
    proxy: EventLoopProxy<()>,
) -> Option<Receiver<Result<Vec<RepoInfo>>>> {
    if token.is_empty() {
        return None;
    }
    let token = token.to_string();
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let client = GitHubClient::new(token);
        let result = client.list_repos(3); // up to 300 repos
        let _ = tx.send(result);
        let _ = proxy.send_event(());
    });

    Some(rx)
}

// --- CI status summary ---

/// Summarized CI status for display in the UI.
#[derive(Debug, Clone)]
pub struct CiStatus {
    /// Overall status: success, failure, pending, or no runs
    pub state: CiState,
    /// Human-readable summary (e.g. "CI passed" or "2/3 checks passed")
    pub summary: String,
    /// URL to open in browser for details
    pub url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiState {
    Success,
    Failure,
    Pending,
    None,
}

impl CiStatus {
    /// Summarize workflow runs into a single CI status.
    fn from_runs(runs: &[WorkflowRun]) -> Self {
        if runs.is_empty() {
            return Self {
                state: CiState::None,
                summary: "No CI runs".into(),
                url: None,
            };
        }

        // Deduplicate: keep only the latest run per workflow name
        let mut latest: std::collections::HashMap<&str, &WorkflowRun> =
            std::collections::HashMap::new();
        for run in runs {
            latest
                .entry(run.name.as_str())
                .and_modify(|existing| {
                    if run.id > existing.id {
                        *existing = run;
                    }
                })
                .or_insert(run);
        }

        let total = latest.len();
        let mut passed = 0;
        let mut failed = 0;
        let mut pending = 0;
        let mut first_url = None;

        for run in latest.values() {
            if first_url.is_none() {
                first_url = Some(run.html_url.clone());
            }
            match run.conclusion.as_deref() {
                Some("success") => passed += 1,
                Some("failure" | "timed_out" | "cancelled") => {
                    failed += 1;
                    // Prefer linking to the failed run
                    first_url = Some(run.html_url.clone());
                }
                _ if run.status == "completed" => passed += 1,
                _ => pending += 1,
            }
        }

        let (state, summary) = if failed > 0 {
            (CiState::Failure, format!("{passed}/{total} checks passed"))
        } else if pending > 0 {
            (CiState::Pending, format!("{pending} check(s) in progress"))
        } else {
            let s = if total == 1 {
                "CI passed".into()
            } else {
                format!("All {total} checks passed")
            };
            (CiState::Success, s)
        };

        Self {
            state,
            summary,
            url: first_url,
        }
    }
}

/// Fetch CI status for a GitHub repo asynchronously.
/// Returns a receiver that will produce a CiStatus once the API call completes.
/// Returns None if the origin remote isn't a GitHub URL or no token is configured.
pub fn fetch_ci_status_async(
    token: &str,
    origin_url: &str,
    branch: Option<String>,
    proxy: EventLoopProxy<()>,
) -> Option<Receiver<CiStatus>> {
    let (owner, repo) = parse_github_remote(origin_url)?;
    let token = token.to_string();
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let client = GitHubClient::new(token);
        let status = match client.workflow_runs(&owner, &repo, branch.as_deref(), 10) {
            Ok(runs) => CiStatus::from_runs(&runs),
            Err(e) => CiStatus {
                state: CiState::None,
                summary: format!("CI fetch failed: {e}"),
                url: None,
            },
        };
        let _ = tx.send(status);
        let _ = proxy.send_event(());
    });

    Some(rx)
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
        let (owner, repo) = parse_github_remote("https://github.com/user/project.git").unwrap();
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

    fn make_run(id: u64, name: &str, status: &str, conclusion: Option<&str>) -> WorkflowRun {
        WorkflowRun {
            id,
            name: name.to_string(),
            head_branch: "main".to_string(),
            head_sha: "abc123".to_string(),
            status: status.to_string(),
            conclusion: conclusion.map(|s| s.to_string()),
            html_url: format!("https://github.com/test/repo/actions/runs/{id}"),
        }
    }

    #[test]
    fn ci_status_all_passed() {
        let runs = vec![
            make_run(1, "CI", "completed", Some("success")),
            make_run(2, "Lint", "completed", Some("success")),
        ];
        let status = CiStatus::from_runs(&runs);
        assert_eq!(status.state, CiState::Success);
    }

    #[test]
    fn ci_status_one_failed() {
        let runs = vec![
            make_run(1, "CI", "completed", Some("success")),
            make_run(2, "Lint", "completed", Some("failure")),
        ];
        let status = CiStatus::from_runs(&runs);
        assert_eq!(status.state, CiState::Failure);
        assert!(status.summary.contains("1/2"));
    }

    #[test]
    fn ci_status_pending() {
        let runs = vec![make_run(1, "CI", "in_progress", None)];
        let status = CiStatus::from_runs(&runs);
        assert_eq!(status.state, CiState::Pending);
    }

    #[test]
    fn ci_status_empty() {
        let status = CiStatus::from_runs(&[]);
        assert_eq!(status.state, CiState::None);
    }

    #[test]
    fn ci_status_deduplicates_by_name() {
        let runs = vec![
            make_run(1, "CI", "completed", Some("failure")),
            make_run(2, "CI", "completed", Some("success")), // newer run replaces old
        ];
        let status = CiStatus::from_runs(&runs);
        assert_eq!(status.state, CiState::Success);
    }
}
