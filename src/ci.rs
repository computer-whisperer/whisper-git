//! Provider-agnostic CI status types.
//!
//! Shared data structures consumed by the UI to display CI results from
//! any provider (GitHub Actions, GitLab CI, etc.).

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiProvider {
    GitHub,
    GitLab,
}

impl CiProvider {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CiState {
    Success,
    Failure,
    Pending,
    #[default]
    None,
}

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

#[derive(Debug, Clone)]
pub struct CiCheckStatus {
    pub label: String,
    pub state: CiState,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CiCommitRollup {
    pub counts: CiCounts,
    pub checks: Vec<CiCheckStatus>,
}

#[derive(Debug, Clone)]
pub struct CiStatus {
    pub state: CiState,
    /// Human-readable summary (e.g. "CI passed" or "2/3 checks passed")
    pub summary: String,
    /// URL to open in browser for details
    pub url: Option<String>,
    pub counts: Option<CiCounts>,
}

#[derive(Debug, Clone)]
pub struct ProviderCiResult {
    pub provider: CiProvider,
    /// Branch-level summary (header bar indicator).
    pub status: CiStatus,
    /// Per-commit provider rollups for compact commit-row rendering.
    pub per_commit_rollups: HashMap<String, CiCommitRollup>,
}

#[derive(Debug, Clone)]
pub struct ProviderCommitRollup {
    pub provider: CiProvider,
    pub rollup: CiCommitRollup,
}

#[derive(Debug, Clone)]
pub struct CiFetchResult {
    pub providers: Vec<ProviderCiResult>,
}

impl CiFetchResult {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_commit_provider_rollups_are_sorted_by_provider() {
        let result = CiFetchResult {
            providers: vec![
                ProviderCiResult {
                    provider: CiProvider::GitLab,
                    status: CiStatus {
                        state: CiState::Failure,
                        summary: "Pipeline failed".into(),
                        url: None,
                        counts: None,
                    },
                    per_commit_rollups: [(
                        "abc".into(),
                        CiCommitRollup {
                            counts: CiCounts {
                                success: 0,
                                failure: 1,
                                pending: 0,
                            },
                            checks: Vec::new(),
                        },
                    )]
                    .into(),
                },
                ProviderCiResult {
                    provider: CiProvider::GitHub,
                    status: CiStatus {
                        state: CiState::Success,
                        summary: "Workflow passed".into(),
                        url: None,
                        counts: None,
                    },
                    per_commit_rollups: [(
                        "abc".into(),
                        CiCommitRollup {
                            counts: CiCounts {
                                success: 1,
                                failure: 0,
                                pending: 0,
                            },
                            checks: Vec::new(),
                        },
                    )]
                    .into(),
                },
            ],
        };

        let grouped = result.per_commit_provider_rollups();
        let rollups = grouped.get("abc").unwrap();
        assert_eq!(rollups.len(), 2);
        assert_eq!(rollups[0].provider, CiProvider::GitHub);
        assert_eq!(rollups[1].provider, CiProvider::GitLab);
    }
}
