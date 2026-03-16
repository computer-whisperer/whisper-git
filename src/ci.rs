//! Provider-agnostic CI status types.
//!
//! Shared data structures consumed by the UI to display CI results from
//! any provider (GitHub Actions, GitLab CI, etc.).

use std::collections::HashMap;

/// Which CI provider produced this result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiProvider {
    GitHub,
    GitLab,
}

impl CiProvider {
    /// Icon key for rendering in the UI.
    pub fn icon(&self) -> &'static str {
        match self {
            CiProvider::GitHub => crate::ui::icon::ICON_GITHUB,
            CiProvider::GitLab => crate::ui::icon::ICON_GITLAB,
        }
    }
}

/// Aggregate CI state for a branch or single commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiState {
    Success,
    Failure,
    Pending,
    None,
}

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

/// CI result from a single provider.
#[derive(Debug, Clone)]
pub struct ProviderCiResult {
    pub provider: CiProvider,
    /// Branch-level summary (for the header bar indicator)
    pub status: CiStatus,
    /// Per-commit CI state keyed by full SHA
    pub per_commit: HashMap<String, CiState>,
}

/// Combined result of CI fetches across all detected providers.
#[derive(Debug, Clone)]
pub struct CiFetchResult {
    pub providers: Vec<ProviderCiResult>,
}

impl CiFetchResult {
    /// Merge per-commit states from all providers.
    /// For each SHA, returns the worst state (Failure > Pending > Success > None).
    pub fn merged_per_commit_states(&self) -> HashMap<String, CiState> {
        let mut merged: HashMap<String, CiState> = HashMap::new();
        for provider in &self.providers {
            for (sha, state) in &provider.per_commit {
                let entry = merged.entry(sha.clone()).or_insert(CiState::None);
                *entry = worse_state(*entry, *state);
            }
        }
        merged
    }
}

/// Return the "worse" of two CI states (Failure > Pending > Success > None).
fn worse_state(a: CiState, b: CiState) -> CiState {
    fn rank(s: CiState) -> u8 {
        match s {
            CiState::None => 0,
            CiState::Success => 1,
            CiState::Pending => 2,
            CiState::Failure => 3,
        }
    }
    if rank(b) > rank(a) { b } else { a }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worse_state_ordering() {
        assert_eq!(worse_state(CiState::Success, CiState::Failure), CiState::Failure);
        assert_eq!(worse_state(CiState::Pending, CiState::Success), CiState::Pending);
        assert_eq!(worse_state(CiState::None, CiState::Success), CiState::Success);
        assert_eq!(worse_state(CiState::Failure, CiState::Pending), CiState::Failure);
    }

    #[test]
    fn merged_per_commit_takes_worst() {
        let result = CiFetchResult {
            providers: vec![
                ProviderCiResult {
                    provider: CiProvider::GitHub,
                    status: CiStatus {
                        state: CiState::Success,
                        summary: "CI passed".into(),
                        url: None,
                    },
                    per_commit: [("abc".into(), CiState::Success)].into(),
                },
                ProviderCiResult {
                    provider: CiProvider::GitLab,
                    status: CiStatus {
                        state: CiState::Failure,
                        summary: "Pipeline failed".into(),
                        url: None,
                    },
                    per_commit: [("abc".into(), CiState::Failure)].into(),
                },
            ],
        };
        let merged = result.merged_per_commit_states();
        assert_eq!(merged["abc"], CiState::Failure);
    }
}
