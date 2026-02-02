use anyhow::{Context, Result};
use git2::{Repository, Commit, Oid};
use std::path::Path;

/// Information about a single commit
#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub id: Oid,
    pub short_id: String,
    pub summary: String,
    pub author: String,
    pub time: i64,
    pub parent_ids: Vec<Oid>,
}

impl CommitInfo {
    fn from_commit(commit: &Commit) -> Self {
        Self {
            id: commit.id(),
            short_id: commit.id().to_string()[..7].to_string(),
            summary: commit.summary().unwrap_or("").to_string(),
            author: commit.author().name().unwrap_or("Unknown").to_string(),
            time: commit.time().seconds(),
            parent_ids: commit.parent_ids().collect(),
        }
    }
}

/// Repository wrapper for our git operations
pub struct GitRepo {
    repo: Repository,
}

impl GitRepo {
    /// Open a repository at the given path
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let repo = Repository::discover(path.as_ref())
            .with_context(|| format!("Failed to open repository at {:?}", path.as_ref()))?;
        Ok(Self { repo })
    }

    /// Get the repository's working directory
    pub fn workdir(&self) -> Option<&Path> {
        self.repo.workdir()
    }

    /// Get recent commits from HEAD
    pub fn recent_commits(&self, count: usize) -> Result<Vec<CommitInfo>> {
        let mut revwalk = self.repo.revwalk().context("Failed to create revwalk")?;
        revwalk.push_head().context("Failed to push HEAD to revwalk")?;

        let commits: Vec<CommitInfo> = revwalk
            .take(count)
            .filter_map(|oid| {
                let oid = oid.ok()?;
                let commit = self.repo.find_commit(oid).ok()?;
                Some(CommitInfo::from_commit(&commit))
            })
            .collect();

        Ok(commits)
    }

    /// Get all branch names
    pub fn branches(&self) -> Result<Vec<String>> {
        let branches = self.repo.branches(None).context("Failed to get branches")?;
        let names: Vec<String> = branches
            .filter_map(|b| {
                let (branch, _) = b.ok()?;
                branch.name().ok()?.map(|s| s.to_string())
            })
            .collect();
        Ok(names)
    }

    /// Get commits for building a graph (includes all branches)
    pub fn commit_graph(&self, max_commits: usize) -> Result<Vec<CommitInfo>> {
        let mut revwalk = self.repo.revwalk().context("Failed to create revwalk")?;

        // Include all branches
        for branch in self.repo.branches(None)? {
            if let Ok((branch, _)) = branch {
                if let Ok(reference) = branch.get().resolve() {
                    if let Some(oid) = reference.target() {
                        let _ = revwalk.push(oid);
                    }
                }
            }
        }

        // Sort topologically for better graph layout
        revwalk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;

        let commits: Vec<CommitInfo> = revwalk
            .take(max_commits)
            .filter_map(|oid| {
                let oid = oid.ok()?;
                let commit = self.repo.find_commit(oid).ok()?;
                Some(CommitInfo::from_commit(&commit))
            })
            .collect();

        Ok(commits)
    }
}
