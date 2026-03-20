//! GitLab REST API client.
//!
//! Provides authenticated access to GitLab's API for fetching pipeline status.
//! Supports both gitlab.com and self-hosted GitLab instances by deriving the
//! API base URL from the remote URL.

use crate::ci::{
    CiCheckStatus, CiCommitRollup, CiCounts, CiProvider, CiState, CiStatus, ProviderCiResult,
};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use winit::event_loop::EventLoopProxy;

/// Parsed GitLab remote: base URL and project path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitLabRemote {
    /// e.g. "https://gitlab.com"
    pub api_base: String,
    /// e.g. "owner/repo" (URL-encoded when used in API calls)
    pub project_path: String,
}

/// Extract GitLab remote info from a remote URL.
/// Supports both HTTPS and SSH formats for gitlab.com and self-hosted instances.
///
/// Detection heuristic: hostname contains "gitlab".
pub fn parse_gitlab_remote(url: &str) -> Option<GitLabRemote> {
    let url = url.trim();

    // SSH: git@gitlab.example.com:group/repo.git
    if let Some(rest) = url.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        if !host.contains("gitlab") {
            return None;
        }
        let path = path.strip_suffix(".git").unwrap_or(path);
        if path.is_empty() || !path.contains('/') {
            return None;
        }
        return Some(GitLabRemote {
            api_base: format!("https://{host}"),
            project_path: path.to_string(),
        });
    }

    // HTTPS: https://gitlab.example.com/group/repo.git
    let stripped = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = stripped.split_once('/')?;
    if !host.contains("gitlab") {
        return None;
    }
    let path = path.strip_suffix(".git").unwrap_or(path);
    // Strip trailing slashes / extra segments like .git/info
    let path = path.split(".git/").next().unwrap_or(path);
    if path.is_empty() || !path.contains('/') {
        return None;
    }
    Some(GitLabRemote {
        api_base: format!("https://{host}"),
        project_path: path.to_string(),
    })
}

// --- Pipeline types ---

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct Pipeline {
    pub id: u64,
    pub sha: String,
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub status: String,
    pub web_url: String,
}

pub struct GitLabClient {
    token: String,
    api_base: String,
}

impl GitLabClient {
    pub fn new(token: String, api_base: String) -> Self {
        Self { token, api_base }
    }

    fn get(&self, path: &str) -> Result<ureq::Response> {
        ureq::get(&format!("{}{path}", self.api_base))
            .set("PRIVATE-TOKEN", &self.token)
            .set("User-Agent", "whisper-git")
            .call()
            .context("GitLab API request failed")
    }

    /// Fetch recent pipelines for a project, optionally filtered by ref (branch).
    pub fn pipelines(
        &self,
        project_path: &str,
        ref_name: Option<&str>,
        per_page: u32,
    ) -> Result<Vec<Pipeline>> {
        let encoded = url_encode_path(project_path);
        let mut path = format!(
            "/api/v4/projects/{encoded}/pipelines?per_page={per_page}&order_by=id&sort=desc"
        );
        if let Some(r) = ref_name {
            path.push_str(&format!("&ref={r}"));
        }
        let resp = self.get(&path)?;
        let pipelines: Vec<Pipeline> = resp.into_json().context("Failed to parse pipelines")?;
        Ok(pipelines)
    }
}

/// URL-encode a GitLab project path (e.g. "group/repo" → "group%2Frepo").
fn url_encode_path(path: &str) -> String {
    path.replace('/', "%2F")
}

// --- CI status computation ---

/// Map a GitLab pipeline status string to CiState.
fn pipeline_state(status: &str) -> CiState {
    match status {
        "success" => CiState::Success,
        "failed" => CiState::Failure,
        "canceled" | "skipped" => CiState::Failure,
        "running"
        | "pending"
        | "created"
        | "waiting_for_resource"
        | "preparing"
        | "scheduled"
        | "manual" => CiState::Pending,
        _ => CiState::None,
    }
}

/// Summarize pipelines into a CiStatus (branch-level summary).
///
/// Pipelines arrive sorted by ID descending (most recent first). We only
/// count pipelines for the most recent commit SHA — older commits' pipelines
/// are historical and shouldn't inflate the pass/fail counts.
fn ci_status_from_pipelines(pipelines: &[Pipeline]) -> CiStatus {
    if pipelines.is_empty() {
        return CiStatus {
            state: CiState::None,
            summary: "No pipelines".into(),
            url: None,
            counts: Some(CiCounts::default()),
        };
    }

    // Only consider pipelines for the most recent commit (first pipeline's SHA).
    let head_sha = &pipelines[0].sha;
    let head_pipelines: Vec<&Pipeline> = pipelines.iter().filter(|p| p.sha == *head_sha).collect();

    let mut passed = 0;
    let mut failed = 0;
    let mut pending = 0;
    let mut first_url = None;

    for p in &head_pipelines {
        if first_url.is_none() {
            first_url = Some(p.web_url.clone());
        }
        match pipeline_state(&p.status) {
            CiState::Success => passed += 1,
            CiState::Failure => {
                failed += 1;
                first_url = Some(p.web_url.clone());
            }
            CiState::Pending => pending += 1,
            CiState::None => {}
        }
    }

    let counts = CiCounts {
        success: passed,
        failure: failed,
        pending,
    };
    let total = counts.total();
    let state = counts.overall_state();
    let summary = match state {
        CiState::Failure => format!(
            "{} failed, {} pending, {} passed",
            counts.failure, counts.pending, counts.success
        ),
        CiState::Pending => format!("{} pending, {} passed", counts.pending, counts.success),
        CiState::Success => {
            if total == 1 {
                "Pipeline passed".into()
            } else {
                format!("All {total} pipelines passed")
            }
        }
        CiState::None => "No pipelines".into(),
    };

    CiStatus {
        state,
        summary,
        url: first_url,
        counts: Some(counts),
    }
}

/// Build per-commit CI states from pipelines.
fn per_commit_rollups(pipelines: &[Pipeline]) -> HashMap<String, CiCommitRollup> {
    let mut by_sha: HashMap<&str, Vec<&Pipeline>> = HashMap::new();
    for p in pipelines {
        by_sha.entry(p.sha.as_str()).or_default().push(p);
    }

    let mut result: HashMap<String, CiCommitRollup> = HashMap::new();
    for (sha, sha_pipelines) in &by_sha {
        // Take the latest pipeline per SHA (highest id)
        if let Some(latest) = sha_pipelines.iter().max_by_key(|p| p.id) {
            let check_state = pipeline_state(&latest.status);
            let checks = vec![CiCheckStatus {
                label: format!("Pipeline #{}", latest.id),
                state: check_state,
                url: Some(latest.web_url.clone()),
            }];
            let counts = CiCounts::from_states(checks.iter().map(|c| c.state));
            result.insert(sha.to_string(), CiCommitRollup { counts, checks });
        }
    }
    result
}

/// Fetch CI status for a GitLab project.
/// Called from a background thread — returns the result directly.
fn fetch_ci_result(token: &str, remote: &GitLabRemote) -> ProviderCiResult {
    let client = GitLabClient::new(token.to_string(), remote.api_base.clone());
    match client.pipelines(&remote.project_path, None, 50) {
        Ok(pipelines) => {
            let per_commit_rollups = per_commit_rollups(&pipelines);
            ProviderCiResult {
                provider: CiProvider::GitLab,
                status: ci_status_from_pipelines(&pipelines),
                per_commit_rollups,
            }
        }
        Err(e) => ProviderCiResult {
            provider: CiProvider::GitLab,
            status: CiStatus {
                state: CiState::None,
                summary: format!("GitLab CI fetch failed: {e}"),
                url: None,
                counts: None,
            },
            per_commit_rollups: HashMap::new(),
        },
    }
}

/// Fetch CI status for a GitLab project asynchronously.
/// Returns None if the URL isn't a GitLab URL.
pub fn fetch_ci_status_async(
    token: &str,
    origin_url: &str,
    proxy: EventLoopProxy<()>,
) -> Option<Receiver<ProviderCiResult>> {
    let remote = parse_gitlab_remote(origin_url)?;
    let token = token.to_string();
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let result = fetch_ci_result(&token, &remote);
        let _ = tx.send(result);
        let _ = proxy.send_event(());
    });

    Some(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ssh_gitlab_com() {
        let r = parse_gitlab_remote("git@gitlab.com:user/project.git").unwrap();
        assert_eq!(r.api_base, "https://gitlab.com");
        assert_eq!(r.project_path, "user/project");
    }

    #[test]
    fn parse_https_gitlab_com() {
        let r = parse_gitlab_remote("https://gitlab.com/user/project.git").unwrap();
        assert_eq!(r.api_base, "https://gitlab.com");
        assert_eq!(r.project_path, "user/project");
    }

    #[test]
    fn parse_self_hosted_ssh() {
        let r = parse_gitlab_remote("git@gitlab.company.com:team/backend/api.git").unwrap();
        assert_eq!(r.api_base, "https://gitlab.company.com");
        assert_eq!(r.project_path, "team/backend/api");
    }

    #[test]
    fn parse_self_hosted_https() {
        let r = parse_gitlab_remote("https://gitlab.internal.io/group/subgroup/repo").unwrap();
        assert_eq!(r.api_base, "https://gitlab.internal.io");
        assert_eq!(r.project_path, "group/subgroup/repo");
    }

    #[test]
    fn parse_non_gitlab() {
        assert!(parse_gitlab_remote("git@github.com:user/repo.git").is_none());
        assert!(parse_gitlab_remote("https://github.com/user/repo").is_none());
    }

    #[test]
    fn pipeline_state_mapping() {
        assert_eq!(pipeline_state("success"), CiState::Success);
        assert_eq!(pipeline_state("failed"), CiState::Failure);
        assert_eq!(pipeline_state("running"), CiState::Pending);
        assert_eq!(pipeline_state("pending"), CiState::Pending);
        assert_eq!(pipeline_state("canceled"), CiState::Failure);
    }

    fn make_pipeline(id: u64, sha: &str, status: &str) -> Pipeline {
        Pipeline {
            id,
            sha: sha.to_string(),
            ref_name: "main".to_string(),
            status: status.to_string(),
            web_url: format!("https://gitlab.com/test/repo/-/pipelines/{id}"),
        }
    }

    #[test]
    fn ci_status_head_commit_passed() {
        // Pipelines sorted by ID descending (most recent first), as the API returns them.
        // Only the head commit (highest ID's SHA) should be counted.
        let pipelines = vec![
            make_pipeline(3, "def", "success"),
            make_pipeline(2, "def", "success"),
            make_pipeline(1, "abc", "failed"), // older commit — ignored
        ];
        let status = ci_status_from_pipelines(&pipelines);
        assert_eq!(status.state, CiState::Success);
        assert_eq!(status.counts.unwrap().success, 2);
    }

    #[test]
    fn ci_status_head_commit_failed() {
        // Head commit has a failure; older commit's success is ignored.
        let pipelines = vec![
            make_pipeline(3, "def", "failed"),
            make_pipeline(2, "abc", "success"), // older commit — ignored
            make_pipeline(1, "abc", "success"), // older commit — ignored
        ];
        let status = ci_status_from_pipelines(&pipelines);
        assert_eq!(status.state, CiState::Failure);
        assert_eq!(status.counts.unwrap().failure, 1);
    }

    #[test]
    fn ci_status_empty() {
        let status = ci_status_from_pipelines(&[]);
        assert_eq!(status.state, CiState::None);
    }

    #[test]
    fn per_commit_latest_wins() {
        let pipelines = vec![
            make_pipeline(1, "abc", "failed"),
            make_pipeline(2, "abc", "success"), // newer
        ];
        let rollups = per_commit_rollups(&pipelines);
        assert_eq!(rollups["abc"].counts.overall_state(), CiState::Success);
    }
}
