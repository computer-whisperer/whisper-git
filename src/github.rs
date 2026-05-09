//! GitHub Actions REST client — feeds CI status into the header bar
//! and per-commit dots in the graph.

use crate::ci::{
    CiCheckStatus, CiCommitRollup, CiCounts, CiProvider, CiState, CiStatus, ProviderCiResult,
};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
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

    if let Some(path) = url.strip_prefix("git@github.com:") {
        let path = path.strip_suffix(".git").unwrap_or(path);
        let (owner, repo) = path.split_once('/')?;
        if !owner.is_empty() && !repo.is_empty() {
            return Some((owner.to_string(), repo.to_string()));
        }
    }

    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    let repo = repo.split('/').next()?;
    if !owner.is_empty() && !repo.is_empty() {
        Some((owner.to_string(), repo.to_string()))
    } else {
        None
    }
}

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

#[derive(Debug, Deserialize)]
struct GitHubApiErrorBody {
    message: Option<String>,
    documentation_url: Option<String>,
}

impl GitHubClient {
    pub fn new(token: String) -> Self {
        Self { token }
    }

    fn classify_http_error(status: u16, body: &str) -> String {
        let parsed = serde_json::from_str::<GitHubApiErrorBody>(body).ok();
        let api_message = parsed
            .as_ref()
            .and_then(|p| p.message.as_ref())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let docs_url = parsed
            .as_ref()
            .and_then(|p| p.documentation_url.as_ref())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let api_message_lc = api_message
            .as_deref()
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();

        let mut message = match status {
            401 => "GitHub API unauthorized (401): token is invalid or expired.".to_string(),
            403 => {
                if api_message_lc.contains("sso")
                    || api_message_lc.contains("saml")
                    || api_message_lc.contains("organization")
                        && api_message_lc.contains("authorize")
                    || api_message_lc.contains("organization")
                        && api_message_lc.contains("grant")
                        && api_message_lc.contains("access")
                {
                    "GitHub API forbidden (403): token is not authorized for organization SSO."
                        .to_string()
                } else if api_message_lc.contains("rate limit") {
                    "GitHub API rate limit exceeded (403).".to_string()
                } else {
                    "GitHub API forbidden (403): token may lack required repository/actions permissions."
                        .to_string()
                }
            }
            404 => {
                "GitHub API returned 404 for this repository. For private or organization repos, this usually means the token does not have access (or is not SSO-authorized).".to_string()
            }
            _ => format!("GitHub API request failed (HTTP {status})."),
        };

        if let Some(api_message) = api_message
            && !api_message.eq_ignore_ascii_case("Not Found")
        {
            message.push_str(&format!(" GitHub says: {api_message}"));
        }
        if let Some(url) = docs_url {
            message.push_str(&format!(" See: {url}"));
        }
        message
    }

    fn map_ureq_error(err: ureq::Error) -> anyhow::Error {
        anyhow::anyhow!("GitHub API transport error: {err}")
    }

    fn ensure_success(resp: &mut ureq::http::Response<ureq::Body>) -> Result<()> {
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        let body = resp.body_mut().read_to_string().unwrap_or_default();
        anyhow::bail!(Self::classify_http_error(status, &body));
    }

    fn get(&self, path: &str) -> Result<ureq::http::Response<ureq::Body>> {
        let mut resp = ureq::get(&format!("{API_BASE}{path}"))
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "whisper-git")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .config()
            .http_status_as_error(false)
            .build()
            .call()
            .map_err(Self::map_ureq_error)?;
        Self::ensure_success(&mut resp)?;
        Ok(resp)
    }

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
        let mut resp = self
            .get(&path)
            .with_context(|| format!("Failed to fetch workflow runs for {owner}/{repo}"))?;
        let body: WorkflowRunsResponse = resp
            .body_mut()
            .read_json()
            .context("Failed to parse runs")?;
        Ok(body.workflow_runs)
    }
}

fn run_state(run: &WorkflowRun) -> CiState {
    match run.conclusion.as_deref() {
        Some("success") => CiState::Success,
        Some("failure" | "timed_out" | "cancelled") => CiState::Failure,
        _ if run.status == "completed" => CiState::Success,
        _ => CiState::Pending,
    }
}

/// Summarize workflow runs into a single CI status.
///
/// Runs arrive sorted by ID descending (most recent first). We only
/// consider runs for the most recent commit SHA — older commits' runs
/// are historical and shouldn't inflate the pass/fail counts.
fn ci_status_from_runs(runs: &[WorkflowRun]) -> CiStatus {
    if runs.is_empty() {
        return CiStatus {
            state: CiState::None,
            summary: "No CI runs".into(),
            url: None,
            counts: Some(CiCounts::default()),
        };
    }

    let head_sha = &runs[0].head_sha;
    let head_runs: Vec<&WorkflowRun> = runs.iter().filter(|r| r.head_sha == *head_sha).collect();

    let mut latest: HashMap<&str, &WorkflowRun> = HashMap::new();
    for run in &head_runs {
        latest
            .entry(run.name.as_str())
            .and_modify(|existing| {
                if run.id > existing.id {
                    *existing = run;
                }
            })
            .or_insert(run);
    }

    let mut checks: Vec<CiCheckStatus> = latest
        .values()
        .map(|run| CiCheckStatus {
            label: run.name.clone(),
            state: run_state(run),
            url: Some(run.html_url.clone()),
        })
        .collect();
    checks.sort_by(|a, b| a.label.cmp(&b.label));

    let counts = CiCounts::from_states(checks.iter().map(|c| c.state));
    let state = counts.overall_state();
    let total = counts.total();
    let summary = match state {
        CiState::Failure => format!(
            "{} failed, {} pending, {} passed",
            counts.failure, counts.pending, counts.success
        ),
        CiState::Pending => format!("{} pending, {} passed", counts.pending, counts.success),
        CiState::Success => {
            if total == 1 {
                "Workflow passed".into()
            } else {
                format!("All {total} workflows passed")
            }
        }
        CiState::None => "No CI runs".into(),
    };
    let first_url = checks
        .iter()
        .find(|c| c.state == CiState::Failure)
        .or_else(|| checks.iter().find(|c| c.state == CiState::Pending))
        .or_else(|| checks.first())
        .and_then(|c| c.url.clone());

    CiStatus {
        state,
        summary,
        url: first_url,
        counts: Some(counts),
    }
}

/// Build per-commit CI states from workflow runs.
fn per_commit_rollups(runs: &[WorkflowRun]) -> HashMap<String, CiCommitRollup> {
    let mut by_sha: HashMap<&str, Vec<&WorkflowRun>> = HashMap::new();
    for run in runs {
        by_sha.entry(run.head_sha.as_str()).or_default().push(run);
    }

    let mut result: HashMap<String, CiCommitRollup> = HashMap::new();
    for (sha, sha_runs) in &by_sha {
        let mut latest: HashMap<&str, &WorkflowRun> = HashMap::new();
        for run in sha_runs {
            latest
                .entry(run.name.as_str())
                .and_modify(|existing| {
                    if run.id > existing.id {
                        *existing = run;
                    }
                })
                .or_insert(run);
        }

        let mut checks: Vec<CiCheckStatus> = latest
            .values()
            .map(|run| CiCheckStatus {
                label: run.name.clone(),
                state: run_state(run),
                url: Some(run.html_url.clone()),
            })
            .collect();
        // Most important states first in compact dot strips.
        checks.sort_by_key(|c| match c.state {
            CiState::Failure => 0,
            CiState::Pending => 1,
            CiState::Success => 2,
            CiState::None => 3,
        });

        let counts = CiCounts::from_states(checks.iter().map(|c| c.state));
        result.insert(sha.to_string(), CiCommitRollup { counts, checks });
    }

    result
}

/// Fetch CI status for a GitHub repo asynchronously. Returns `None` when
/// `origin_url` doesn't parse as a GitHub URL.
pub fn fetch_ci_status_async(
    token: &str,
    origin_url: &str,
    proxy: EventLoopProxy<()>,
) -> Option<Receiver<ProviderCiResult>> {
    let (owner, repo) = parse_github_remote(origin_url)?;
    let token = token.to_string();
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let client = GitHubClient::new(token);
        let result = match client.workflow_runs(&owner, &repo, None, 50) {
            Ok(runs) => {
                let per_commit_rollups = per_commit_rollups(&runs);
                ProviderCiResult {
                    provider: CiProvider::GitHub,
                    status: ci_status_from_runs(&runs),
                    per_commit_rollups,
                }
            }
            Err(e) => ProviderCiResult {
                provider: CiProvider::GitHub,
                status: CiStatus {
                    state: CiState::None,
                    summary: format!("CI fetch failed: {e}"),
                    url: None,
                    counts: None,
                },
                per_commit_rollups: HashMap::new(),
            },
        };
        let _ = tx.send(result);
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
        let status = ci_status_from_runs(&runs);
        assert_eq!(status.state, CiState::Success);
    }

    #[test]
    fn ci_status_one_failed() {
        let runs = vec![
            make_run(1, "CI", "completed", Some("success")),
            make_run(2, "Lint", "completed", Some("failure")),
        ];
        let status = ci_status_from_runs(&runs);
        assert_eq!(status.state, CiState::Failure);
        assert!(status.summary.contains("failed"));
    }

    #[test]
    fn ci_status_pending() {
        let runs = vec![make_run(1, "CI", "in_progress", None)];
        let status = ci_status_from_runs(&runs);
        assert_eq!(status.state, CiState::Pending);
    }

    #[test]
    fn ci_status_empty() {
        let status = ci_status_from_runs(&[]);
        assert_eq!(status.state, CiState::None);
    }

    #[test]
    fn ci_status_deduplicates_by_name() {
        let runs = vec![
            make_run(1, "CI", "completed", Some("failure")),
            make_run(2, "CI", "completed", Some("success")),
        ];
        let status = ci_status_from_runs(&runs);
        assert_eq!(status.state, CiState::Success);
    }

    #[test]
    fn ci_status_ignores_older_commit_runs() {
        let mut old_fail = make_run(1, "CI", "completed", Some("failure"));
        old_fail.head_sha = "old_sha".to_string();
        let runs = vec![
            make_run(3, "CI", "completed", Some("success")),
            make_run(2, "Lint", "completed", Some("success")),
            old_fail,
        ];
        let status = ci_status_from_runs(&runs);
        assert_eq!(status.state, CiState::Success);
        assert_eq!(status.counts.unwrap().success, 2);
    }

    #[test]
    fn classify_404_mentions_private_org_access() {
        let body = r#"{"message":"Not Found","documentation_url":"https://docs.github.com/rest"}"#;
        let msg = GitHubClient::classify_http_error(404, body);
        assert!(msg.contains("private or organization repos"));
        assert!(msg.contains("token does not have access"));
        assert!(msg.contains("docs.github.com/rest"));
    }

    #[test]
    fn classify_403_sso_message_is_explicit() {
        let body = r#"{"message":"Resource protected by organization SAML enforcement. You must grant your Personal Access token access to this organization.","documentation_url":"https://docs.github.com/rest"}"#;
        let msg = GitHubClient::classify_http_error(403, body);
        assert!(msg.contains("organization SSO"));
    }
}
