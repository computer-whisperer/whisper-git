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

    /// Short provider label used in compact UI badges.
    pub fn short_label(&self) -> &'static str {
        match self {
            CiProvider::GitHub => "GH",
            CiProvider::GitLab => "GL",
        }
    }

    /// Stable provider sort key for deterministic UI ordering.
    pub fn sort_key(&self) -> u8 {
        match self {
            CiProvider::GitHub => 0,
            CiProvider::GitLab => 1,
        }
    }
}

/// Aggregate CI state for a branch or single commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CiState {
    Success,
    Failure,
    Pending,
    #[default]
    None,
}

/// Simple CI state counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CiCounts {
    pub success: usize,
    pub failure: usize,
    pub pending: usize,
}

impl CiCounts {
    pub fn total(&self) -> usize {
        self.success + self.failure + self.pending
    }

    pub fn from_states(states: impl IntoIterator<Item = CiState>) -> Self {
        let mut counts = Self::default();
        for state in states {
            match state {
                CiState::Success => counts.success += 1,
                CiState::Failure => counts.failure += 1,
                CiState::Pending => counts.pending += 1,
                CiState::None => {}
            }
        }
        counts
    }

    pub fn overall_state(&self) -> CiState {
        if self.failure > 0 {
            CiState::Failure
        } else if self.pending > 0 {
            CiState::Pending
        } else if self.success > 0 {
            CiState::Success
        } else {
            CiState::None
        }
    }
}

/// One concrete pipeline/workflow check state.
#[derive(Debug, Clone)]
pub struct CiCheckStatus {
    pub label: String,
    pub state: CiState,
    pub url: Option<String>,
}

/// Per-commit rollup for one provider.
#[derive(Debug, Clone, Default)]
pub struct CiCommitRollup {
    pub state: CiState,
    pub counts: CiCounts,
    pub checks: Vec<CiCheckStatus>,
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
    /// Structured counts for richer UI summaries
    pub counts: Option<CiCounts>,
}

/// CI result from a single provider.
#[derive(Debug, Clone)]
pub struct ProviderCiResult {
    pub provider: CiProvider,
    /// Branch-level summary (for the header bar indicator)
    pub status: CiStatus,
    /// Per-commit CI state keyed by full SHA
    pub per_commit: HashMap<String, CiState>,
    /// Per-commit provider rollups for compact commit-row rendering.
    pub per_commit_rollups: HashMap<String, CiCommitRollup>,
}

/// Provider-scoped CI rollup for one commit (used in graph rows).
#[derive(Debug, Clone)]
pub struct ProviderCommitRollup {
    pub provider: CiProvider,
    pub rollup: CiCommitRollup,
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

    /// Group per-commit rollups by SHA with provider attribution.
    pub fn per_commit_provider_rollups(&self) -> HashMap<String, Vec<ProviderCommitRollup>> {
        let mut grouped: HashMap<String, Vec<ProviderCommitRollup>> = HashMap::new();
        for provider in &self.providers {
            for (sha, rollup) in &provider.per_commit_rollups {
                grouped
                    .entry(sha.clone())
                    .or_default()
                    .push(ProviderCommitRollup {
                        provider: provider.provider,
                        rollup: rollup.clone(),
                    });
            }
        }
        for rollups in grouped.values_mut() {
            rollups.sort_by_key(|r| r.provider.sort_key());
        }
        grouped
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
        assert_eq!(
            worse_state(CiState::Success, CiState::Failure),
            CiState::Failure
        );
        assert_eq!(
            worse_state(CiState::Pending, CiState::Success),
            CiState::Pending
        );
        assert_eq!(
            worse_state(CiState::None, CiState::Success),
            CiState::Success
        );
        assert_eq!(
            worse_state(CiState::Failure, CiState::Pending),
            CiState::Failure
        );
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
                        counts: None,
                    },
                    per_commit: [("abc".into(), CiState::Success)].into(),
                    per_commit_rollups: HashMap::new(),
                },
                ProviderCiResult {
                    provider: CiProvider::GitLab,
                    status: CiStatus {
                        state: CiState::Failure,
                        summary: "Pipeline failed".into(),
                        url: None,
                        counts: None,
                    },
                    per_commit: [("abc".into(), CiState::Failure)].into(),
                    per_commit_rollups: HashMap::new(),
                },
            ],
        };
        let merged = result.merged_per_commit_states();
        assert_eq!(merged["abc"], CiState::Failure);
    }
}
