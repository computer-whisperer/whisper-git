use anyhow::{Context, Result};
use git2::{Repository, Commit, Oid, Status, StatusOptions};
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

    /// Get the repository name (basename of workdir or bare repo path)
    pub fn repo_name(&self) -> String {
        self.repo
            .workdir()
            .or_else(|| self.repo.path().parent())
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string()
    }

    /// Get the current branch name
    pub fn current_branch(&self) -> Result<String> {
        let head = self.repo.head().context("Failed to get HEAD")?;
        if head.is_branch() {
            Ok(head
                .shorthand()
                .unwrap_or("HEAD")
                .to_string())
        } else {
            // Detached HEAD - show short commit id
            Ok(head
                .target()
                .map(|oid| oid.to_string()[..7].to_string())
                .unwrap_or_else(|| "HEAD".to_string()))
        }
    }

    /// Get the head commit OID
    pub fn head_oid(&self) -> Result<Oid> {
        let head = self.repo.head().context("Failed to get HEAD")?;
        head.target().context("HEAD has no target")
    }

    /// Get ahead/behind count relative to upstream
    pub fn ahead_behind(&self) -> Result<(usize, usize)> {
        let head = self.repo.head().context("Failed to get HEAD")?;
        if !head.is_branch() {
            return Ok((0, 0));
        }

        let branch_name = head.shorthand().unwrap_or("HEAD");
        let local_branch = self.repo.find_branch(branch_name, git2::BranchType::Local)
            .context("Failed to find local branch")?;

        let upstream = match local_branch.upstream() {
            Ok(u) => u,
            Err(_) => return Ok((0, 0)), // No upstream configured
        };

        let local_oid = head.target().context("HEAD has no target")?;
        let upstream_oid = upstream
            .get()
            .target()
            .context("Upstream has no target")?;

        let (ahead, behind) = self.repo
            .graph_ahead_behind(local_oid, upstream_oid)
            .context("Failed to compute ahead/behind")?;

        Ok((ahead, behind))
    }

    /// Get working directory status
    pub fn status(&self) -> Result<WorkingDirStatus> {
        let mut opts = StatusOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true);

        let statuses = self.repo.statuses(Some(&mut opts))
            .context("Failed to get status")?;

        let mut staged = Vec::new();
        let mut unstaged = Vec::new();

        for entry in statuses.iter() {
            let path = entry.path().unwrap_or("").to_string();
            let status = entry.status();

            // Check for staged changes
            if status.intersects(
                Status::INDEX_NEW
                    | Status::INDEX_MODIFIED
                    | Status::INDEX_DELETED
                    | Status::INDEX_RENAMED
                    | Status::INDEX_TYPECHANGE,
            ) {
                staged.push(FileStatus {
                    path: path.clone(),
                    status: FileStatusKind::from_index_status(status),
                });
            }

            // Check for unstaged changes
            if status.intersects(
                Status::WT_NEW
                    | Status::WT_MODIFIED
                    | Status::WT_DELETED
                    | Status::WT_RENAMED
                    | Status::WT_TYPECHANGE,
            ) {
                unstaged.push(FileStatus {
                    path,
                    status: FileStatusKind::from_wt_status(status),
                });
            }
        }

        Ok(WorkingDirStatus { staged, unstaged })
    }

    /// Stage a file
    pub fn stage_file(&self, path: &str) -> Result<()> {
        let mut index = self.repo.index().context("Failed to get index")?;
        index.add_path(Path::new(path)).context("Failed to stage file")?;
        index.write().context("Failed to write index")?;
        Ok(())
    }

    /// Unstage a file
    pub fn unstage_file(&self, path: &str) -> Result<()> {
        let head = self.repo.head().context("Failed to get HEAD")?;
        let head_commit = head.peel_to_commit().context("Failed to get HEAD commit")?;
        self.repo
            .reset_default(Some(&head_commit.as_object()), [Path::new(path)])
            .context("Failed to unstage file")?;

        Ok(())
    }

    /// Create a commit with the staged changes
    pub fn commit(&self, message: &str) -> Result<Oid> {
        let mut index = self.repo.index().context("Failed to get index")?;
        let tree_oid = index.write_tree().context("Failed to write tree")?;
        let tree = self.repo.find_tree(tree_oid).context("Failed to find tree")?;

        let head = self.repo.head().context("Failed to get HEAD")?;
        let parent_commit = head.peel_to_commit().context("Failed to get parent commit")?;

        let sig = self.repo.signature().context("Failed to get signature")?;

        let commit_oid = self.repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                message,
                &tree,
                &[&parent_commit],
            )
            .context("Failed to create commit")?;

        Ok(commit_oid)
    }

    /// Get diff stats for a file
    pub fn diff_file_stats(&self, path: &str, staged: bool) -> Result<(usize, usize)> {
        let diff = if staged {
            let head = self.repo.head().context("Failed to get HEAD")?;
            let head_tree = head.peel_to_tree().context("Failed to get HEAD tree")?;
            self.repo.diff_tree_to_index(
                Some(&head_tree),
                Some(&self.repo.index()?),
                None,
            )?
        } else {
            self.repo.diff_index_to_workdir(None, None)?
        };

        let mut additions = 0;
        let mut deletions = 0;

        diff.foreach(
            &mut |delta, _| {
                if let Some(p) = delta.new_file().path() {
                    if p.to_str() == Some(path) {
                        return true;
                    }
                }
                if let Some(p) = delta.old_file().path() {
                    if p.to_str() == Some(path) {
                        return true;
                    }
                }
                true
            },
            None,
            None,
            Some(&mut |delta, _hunk, line| {
                let check_path = |p: Option<&Path>| p.and_then(|p| p.to_str()) == Some(path);
                if check_path(delta.new_file().path()) || check_path(delta.old_file().path()) {
                    match line.origin() {
                        '+' => additions += 1,
                        '-' => deletions += 1,
                        _ => {}
                    }
                }
                true
            }),
        )?;

        Ok((additions, deletions))
    }

    /// Get submodules
    pub fn submodules(&self) -> Result<Vec<SubmoduleInfo>> {
        let submodules = self.repo.submodules().context("Failed to get submodules")?;

        let mut infos = Vec::new();
        for sm in submodules {
            let name = sm.name().unwrap_or("unknown").to_string();
            let path = sm.path().to_string_lossy().to_string();

            // Try to open the submodule to get more info
            let (branch, is_dirty) = if let Ok(sub_repo) = sm.open() {
                let branch = sub_repo
                    .head()
                    .ok()
                    .and_then(|h| h.shorthand().map(|s| s.to_string()))
                    .unwrap_or_else(|| "detached".to_string());

                let is_dirty = sub_repo
                    .statuses(None)
                    .map(|s| !s.is_empty())
                    .unwrap_or(false);

                (branch, is_dirty)
            } else {
                ("unknown".to_string(), false)
            };

            infos.push(SubmoduleInfo {
                name,
                path,
                branch,
                is_dirty,
            });
        }

        Ok(infos)
    }

    /// Get worktrees
    pub fn worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        let worktrees = self.repo.worktrees().context("Failed to get worktrees")?;

        // Get the current working directory for comparison
        let current_workdir = self.repo.workdir().map(|p| p.to_path_buf());

        let mut infos = Vec::new();
        for name in worktrees.iter() {
            if let Some(name) = name {
                if let Ok(wt) = self.repo.find_worktree(name) {
                    let wt_path = wt.path();
                    let path = wt_path.to_string_lossy().to_string();

                    // Check if this is the current worktree
                    let is_current = current_workdir
                        .as_ref()
                        .map(|cwd| cwd == wt_path)
                        .unwrap_or(false);

                    // Try to get branch info
                    let branch = if let Ok(wt_repo) = Repository::open(wt_path) {
                        wt_repo
                            .head()
                            .ok()
                            .and_then(|h| h.shorthand().map(|s| s.to_string()))
                            .unwrap_or_else(|| "detached".to_string())
                    } else {
                        "unknown".to_string()
                    };

                    infos.push(WorktreeInfo {
                        name: name.to_string(),
                        path,
                        branch,
                        is_current,
                    });
                }
            }
        }

        Ok(infos)
    }

    /// Get branch tips (for graph labels)
    pub fn branch_tips(&self) -> Result<Vec<BranchTip>> {
        let mut tips = Vec::new();

        for branch in self.repo.branches(None)? {
            if let Ok((branch, branch_type)) = branch {
                if let Ok(reference) = branch.get().resolve() {
                    if let Some(oid) = reference.target() {
                        let name = branch.name().ok().flatten().unwrap_or("").to_string();
                        let is_remote = branch_type == git2::BranchType::Remote;
                        let is_head = self.repo.head().ok()
                            .and_then(|h| h.target())
                            .map(|h| h == oid)
                            .unwrap_or(false);

                        tips.push(BranchTip {
                            name,
                            oid,
                            is_remote,
                            is_head,
                        });
                    }
                }
            }
        }

        Ok(tips)
    }

    /// Get tags
    pub fn tags(&self) -> Result<Vec<TagInfo>> {
        let mut tags = Vec::new();

        self.repo.tag_foreach(|oid, name| {
            let name = String::from_utf8_lossy(name)
                .trim_start_matches("refs/tags/")
                .to_string();

            // Resolve to the commit (tags can point to tag objects)
            let commit_oid = self.repo
                .find_object(oid, None)
                .ok()
                .and_then(|obj| obj.peel_to_commit().ok())
                .map(|c| c.id())
                .unwrap_or(oid);

            tags.push(TagInfo {
                name,
                oid: commit_oid,
            });
            true
        })?;

        Ok(tags)
    }
}

/// Working directory status
#[derive(Clone, Debug, Default)]
pub struct WorkingDirStatus {
    pub staged: Vec<FileStatus>,
    pub unstaged: Vec<FileStatus>,
}

impl WorkingDirStatus {
    pub fn is_clean(&self) -> bool {
        self.staged.is_empty() && self.unstaged.is_empty()
    }

    pub fn total_files(&self) -> usize {
        self.staged.len() + self.unstaged.len()
    }
}

/// Status of a single file
#[derive(Clone, Debug)]
pub struct FileStatus {
    pub path: String,
    pub status: FileStatusKind,
}

/// Kind of file status change
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatusKind {
    New,
    Modified,
    Deleted,
    Renamed,
    TypeChange,
}

impl FileStatusKind {
    fn from_index_status(status: Status) -> Self {
        if status.contains(Status::INDEX_NEW) {
            FileStatusKind::New
        } else if status.contains(Status::INDEX_MODIFIED) {
            FileStatusKind::Modified
        } else if status.contains(Status::INDEX_DELETED) {
            FileStatusKind::Deleted
        } else if status.contains(Status::INDEX_RENAMED) {
            FileStatusKind::Renamed
        } else {
            FileStatusKind::TypeChange
        }
    }

    fn from_wt_status(status: Status) -> Self {
        if status.contains(Status::WT_NEW) {
            FileStatusKind::New
        } else if status.contains(Status::WT_MODIFIED) {
            FileStatusKind::Modified
        } else if status.contains(Status::WT_DELETED) {
            FileStatusKind::Deleted
        } else if status.contains(Status::WT_RENAMED) {
            FileStatusKind::Renamed
        } else {
            FileStatusKind::TypeChange
        }
    }

    pub fn symbol(&self) -> char {
        match self {
            FileStatusKind::New => 'A',
            FileStatusKind::Modified => 'M',
            FileStatusKind::Deleted => 'D',
            FileStatusKind::Renamed => 'R',
            FileStatusKind::TypeChange => 'T',
        }
    }
}

/// Submodule information
#[derive(Clone, Debug)]
pub struct SubmoduleInfo {
    pub name: String,
    pub path: String,
    pub branch: String,
    pub is_dirty: bool,
}

/// Worktree information
#[derive(Clone, Debug)]
pub struct WorktreeInfo {
    pub name: String,
    pub path: String,
    pub branch: String,
    pub is_current: bool,
}

/// Branch tip for graph labels
#[derive(Clone, Debug)]
pub struct BranchTip {
    pub name: String,
    pub oid: Oid,
    pub is_remote: bool,
    pub is_head: bool,
}

/// Tag information
#[derive(Clone, Debug)]
pub struct TagInfo {
    pub name: String,
    pub oid: Oid,
}
