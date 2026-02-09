use anyhow::{Context, Result};
use git2::{Diff, Repository, Commit, Oid, Status, StatusOptions};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

/// Byte offset ranges for intra-line diff highlighting
type DiffRanges = (Vec<(usize, usize)>, Vec<(usize, usize)>);

/// Format a Unix timestamp as a human-readable relative time string.
pub fn format_relative_time(timestamp: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let diff = (now - timestamp).max(0);
    match diff {
        d if d < 60 => "just now".to_string(),
        d if d < 3600 => format!("{}m", d / 60),
        d if d < 86400 => format!("{}h", d / 3600),
        d if d < 604800 => format!("{}d", d / 86400),
        d if d < 2592000 => format!("{}w", d / 604800),
        d if d < 31536000 => format!("{}mo", d / 2592000),
        d => format!("{}y", d / 31536000),
    }
}

/// Information about a single commit
#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub id: Oid,
    pub short_id: String,
    pub summary: String,
    pub author: String,
    pub author_email: String,
    pub time: i64,
    pub parent_ids: Vec<Oid>,
}

impl CommitInfo {
    fn from_commit(commit: &Commit) -> Self {
        Self {
            id: commit.id(),
            short_id: commit.id().to_string().get(..7).unwrap_or("").to_string(),
            summary: commit.summary().unwrap_or("").to_string(),
            author: commit.author().name().unwrap_or("Unknown").to_string(),
            author_email: commit.author().email().unwrap_or("").to_string(),
            time: commit.time().seconds(),
            parent_ids: commit.parent_ids().collect(),
        }
    }

    pub fn relative_time(&self) -> String {
        format_relative_time(self.time)
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

    /// Get commits for building a graph (includes all branches)
    pub fn commit_graph(&self, max_commits: usize) -> Result<Vec<CommitInfo>> {
        let mut revwalk = self.repo.revwalk().context("Failed to create revwalk")?;

        // Include all branches
        for branch in self.repo.branches(None)? {
            if let Ok((branch, _)) = branch
                && let Ok(reference) = branch.get().resolve()
                    && let Some(oid) = reference.target() {
                        let _ = revwalk.push(oid);
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
        if let Some(workdir) = self.repo.workdir() {
            return workdir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();
        }

        // Bare repo: path() returns e.g. "/project/.bare/" -- walk up to find a
        // non-hidden directory name that represents the project.
        let mut dir = self.repo.path();
        loop {
            match dir.file_name().and_then(|n| n.to_str()) {
                Some(name) if !name.starts_with('.') => return name.to_string(),
                _ => match dir.parent() {
                    Some(parent) if parent != dir => dir = parent,
                    _ => return "unknown".to_string(),
                },
            }
        }
    }

    /// Get the current branch name
    pub fn current_branch(&self) -> Result<String> {
        match self.repo.head() {
            Ok(head) => {
                if head.is_branch() {
                    Ok(head.shorthand().unwrap_or("HEAD").to_string())
                } else {
                    // Detached HEAD - show short commit id
                    Ok(head
                        .target()
                        .map(|oid| oid.to_string().get(..7).unwrap_or("").to_string())
                        .unwrap_or_else(|| "HEAD".to_string()))
                }
            }
            Err(_) => {
                // HEAD points to a non-existent branch (common in bare repos).
                // Fall back to the first local branch we can find.
                if let Ok(branches) = self.repo.branches(Some(git2::BranchType::Local)) {
                    for branch in branches {
                        if let Ok((branch, _)) = branch
                            && let Ok(Some(name)) = branch.name() {
                                return Ok(name.to_string());
                            }
                    }
                }
                Ok("HEAD".to_string())
            }
        }
    }

    /// Get the head commit OID (falls back to first local branch tip for bare repos)
    pub fn head_oid(&self) -> Result<Oid> {
        if let Ok(head) = self.repo.head()
            && let Some(oid) = head.target() {
                return Ok(oid);
            }
        // Fallback: first local branch tip
        for branch in self.repo.branches(Some(git2::BranchType::Local))? {
            if let Ok((branch, _)) = branch
                && let Ok(reference) = branch.get().resolve()
                    && let Some(oid) = reference.target() {
                        return Ok(oid);
                    }
        }
        anyhow::bail!("No HEAD or branch tips found")
    }

    /// Get ahead/behind count relative to upstream
    pub fn ahead_behind(&self) -> Result<(usize, usize)> {
        let head = match self.repo.head() {
            Ok(h) => h,
            Err(_) => return Ok((0, 0)), // Bare repo with stale HEAD
        };
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
        if self.repo.is_bare() {
            return Ok(WorkingDirStatus::default());
        }
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
        if self.repo.is_bare() {
            anyhow::bail!("Cannot perform this operation on a bare repository");
        }
        let mut index = self.repo.index().context("Failed to get index")?;
        index.add_path(Path::new(path)).context("Failed to stage file")?;
        index.write().context("Failed to write index")?;
        Ok(())
    }

    /// Unstage a file
    pub fn unstage_file(&self, path: &str) -> Result<()> {
        if self.repo.is_bare() {
            anyhow::bail!("Cannot perform this operation on a bare repository");
        }
        let head = self.repo.head().context("Failed to get HEAD")?;
        let head_commit = head.peel_to_commit().context("Failed to get HEAD commit")?;
        self.repo
            .reset_default(Some(head_commit.as_object()), [Path::new(path)])
            .context("Failed to unstage file")?;

        Ok(())
    }

    /// Create a commit with the staged changes
    pub fn commit(&self, message: &str) -> Result<Oid> {
        if self.repo.is_bare() {
            anyhow::bail!("Cannot perform this operation on a bare repository");
        }
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
            if let Some(name) = name
                && let Ok(wt) = self.repo.find_worktree(name) {
                    let wt_path = wt.path();
                    let path = wt_path.to_string_lossy().to_string();

                    // Check if this is the current worktree
                    let is_current = current_workdir
                        .as_ref()
                        .map(|cwd| cwd == wt_path)
                        .unwrap_or(false);

                    // Try to get branch info and dirty status
                    let (branch, is_dirty) = if let Ok(wt_repo) = Repository::open(wt_path) {
                        let branch = wt_repo
                            .head()
                            .ok()
                            .and_then(|h| h.shorthand().map(|s| s.to_string()))
                            .unwrap_or_else(|| "detached".to_string());

                        let is_dirty = wt_repo
                            .statuses(None)
                            .map(|statuses| {
                                statuses.iter().any(|entry| {
                                    !entry.status().intersects(Status::IGNORED)
                                })
                            })
                            .unwrap_or(false);

                        (branch, is_dirty)
                    } else {
                        ("unknown".to_string(), false)
                    };

                    infos.push(WorktreeInfo {
                        name: name.to_string(),
                        path,
                        branch,
                        is_current,
                        is_dirty,
                    });
                }
        }

        Ok(infos)
    }

    /// Get branch tips (for graph labels)
    pub fn branch_tips(&self) -> Result<Vec<BranchTip>> {
        let head_oid = self.repo.head().ok().and_then(|h| h.target());
        let mut tips = Vec::new();

        for branch in self.repo.branches(None)? {
            if let Ok((branch, branch_type)) = branch
                && let Ok(reference) = branch.get().resolve()
                    && let Some(oid) = reference.target() {
                        let name = branch.name().ok().flatten().unwrap_or("").to_string();
                        let is_remote = branch_type == git2::BranchType::Remote;
                        let is_head = head_oid == Some(oid);

                        tips.push(BranchTip {
                            name,
                            oid,
                            is_remote,
                            is_head,
                        });
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

    /// Get the diff for a commit compared to its first parent
    pub fn diff_for_commit(&self, oid: Oid) -> Result<Vec<DiffFile>> {
        let commit = self.repo.find_commit(oid)
            .context("Failed to find commit")?;
        let tree = commit.tree().context("Failed to get commit tree")?;

        let parent_tree = if commit.parent_count() > 0 {
            let parent = commit.parent(0).context("Failed to get parent commit")?;
            Some(parent.tree().context("Failed to get parent tree")?)
        } else {
            None
        };

        let diff = self.repo.diff_tree_to_tree(
            parent_tree.as_ref(),
            Some(&tree),
            None,
        ).context("Failed to compute diff")?;

        Self::parse_diff(&diff)
    }

    /// Get the diff hunks for a working directory file (staged or unstaged)
    pub fn diff_working_file(&self, path: &str, staged: bool) -> Result<Vec<DiffHunk>> {
        let mut opts = git2::DiffOptions::new();
        opts.pathspec(path);

        let diff = if staged {
            let head = self.repo.head().context("Failed to get HEAD")?;
            let head_tree = head.peel_to_tree().context("Failed to get HEAD tree")?;
            self.repo.diff_tree_to_index(
                Some(&head_tree),
                Some(&self.repo.index()?),
                Some(&mut opts),
            )?
        } else {
            self.repo.diff_index_to_workdir(None, Some(&mut opts))?
        };

        let files = Self::parse_diff(&diff)?;
        Ok(files.into_iter().flat_map(|f| f.hunks).collect())
    }

    /// Compute intra-line highlight ranges for paired add/remove lines within hunks.
    /// Finds consecutive `-` then `+` line pairs and highlights the differing byte ranges.
    fn compute_intra_line_highlights(files: &mut [DiffFile]) {
        for file in files.iter_mut() {
            for hunk in &mut file.hunks {
                // Find paired -/+ line runs within the hunk
                let len = hunk.lines.len();
                let mut i = 0;
                while i < len {
                    // Collect a run of '-' lines followed by a run of '+' lines
                    let del_start = i;
                    while i < len && hunk.lines[i].origin == '-' {
                        i += 1;
                    }
                    let del_end = i;

                    let add_start = i;
                    while i < len && hunk.lines[i].origin == '+' {
                        i += 1;
                    }
                    let add_end = i;

                    let del_count = del_end - del_start;
                    let add_count = add_end - add_start;

                    // Only compute highlights if we have paired lines
                    if del_count > 0 && add_count > 0 {
                        let pair_count = del_count.min(add_count);
                        for j in 0..pair_count {
                            let del_idx = del_start + j;
                            let add_idx = add_start + j;
                            let (del_ranges, add_ranges) = Self::diff_chars(
                                &hunk.lines[del_idx].content,
                                &hunk.lines[add_idx].content,
                            );
                            hunk.lines[del_idx].highlight_ranges = del_ranges;
                            hunk.lines[add_idx].highlight_ranges = add_ranges;
                        }
                    }

                    // Skip context lines
                    if i == del_end && i == add_start {
                        i += 1;
                    }
                }
            }
        }
    }

    /// Compute the differing byte ranges between two strings.
    /// Returns (old_ranges, new_ranges) where each range is a (start, end) byte offset
    /// into the respective string's content (excluding trailing newline).
    fn diff_chars(old: &str, new: &str) -> DiffRanges {
        let old = old.trim_end_matches('\n');
        let new = new.trim_end_matches('\n');

        let old_bytes = old.as_bytes();
        let new_bytes = new.as_bytes();

        // Find common prefix length
        let prefix_len = old_bytes.iter()
            .zip(new_bytes.iter())
            .take_while(|(a, b)| a == b)
            .count();

        // Find common suffix length (not overlapping with prefix)
        let old_remaining = old_bytes.len() - prefix_len;
        let new_remaining = new_bytes.len() - prefix_len;
        let suffix_len = old_bytes[prefix_len..].iter().rev()
            .zip(new_bytes[prefix_len..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count()
            .min(old_remaining)
            .min(new_remaining);

        let old_diff_end = old_bytes.len() - suffix_len;
        let new_diff_end = new_bytes.len() - suffix_len;

        // If the entire line changed or nothing changed, return empty (render as full-line highlight)
        if prefix_len == 0 && suffix_len == 0 {
            return (Vec::new(), Vec::new());
        }
        if prefix_len >= old_diff_end && prefix_len >= new_diff_end {
            // Lines are identical
            return (Vec::new(), Vec::new());
        }

        let old_ranges = if prefix_len < old_diff_end {
            vec![(prefix_len, old_diff_end)]
        } else {
            Vec::new()
        };
        let new_ranges = if prefix_len < new_diff_end {
            vec![(prefix_len, new_diff_end)]
        } else {
            Vec::new()
        };

        (old_ranges, new_ranges)
    }

    fn parse_diff(diff: &Diff) -> Result<Vec<DiffFile>> {
        let mut files: Vec<DiffFile> = Vec::new();

        diff.print(git2::DiffFormat::Patch, |delta, hunk, line| {
            let path = delta.new_file().path()
                .or_else(|| delta.old_file().path())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();

            // Create a new file entry if the path changed
            let need_new_file = files.last().map(|f: &DiffFile| f.path != path).unwrap_or(true);
            if need_new_file {
                files.push(DiffFile {
                    path,
                    hunks: Vec::new(),
                    additions: 0,
                    deletions: 0,
                });
            }

            let file = files.last_mut().unwrap();
            let origin = line.origin();

            match origin {
                'F' | 'H' => {
                    // File header or hunk header
                    if origin == 'H' {
                        let header = hunk.map(|h| {
                            String::from_utf8_lossy(h.header()).trim_end().to_string()
                        }).unwrap_or_default();
                        file.hunks.push(DiffHunk {
                            header,
                            lines: Vec::new(),
                        });
                    }
                }
                '+' | '-' | ' ' => {
                    match origin {
                        '+' => file.additions += 1,
                        '-' => file.deletions += 1,
                        _ => {}
                    }
                    // Create default hunk if none exists yet
                    if file.hunks.is_empty() {
                        file.hunks.push(DiffHunk {
                            header: String::new(),
                            lines: Vec::new(),
                        });
                    }
                    if let Some(hunk) = file.hunks.last_mut() {
                        hunk.lines.push(DiffLine {
                            origin,
                            content: String::from_utf8_lossy(line.content()).to_string(),
                            old_lineno: line.old_lineno(),
                            new_lineno: line.new_lineno(),
                            highlight_ranges: Vec::new(),
                        });
                    }
                }
                _ => {}
            }
            true
        })?;

        Self::compute_intra_line_highlights(&mut files);
        Ok(files)
    }
}

/// A file changed in a diff, with its hunks
#[derive(Clone, Debug)]
pub struct DiffFile {
    pub path: String,
    pub hunks: Vec<DiffHunk>,
    pub additions: usize,
    pub deletions: usize,
}

impl DiffFile {
    /// Build a DiffFile from a path and hunks, computing addition/deletion counts.
    pub fn from_hunks(path: String, hunks: Vec<DiffHunk>) -> Self {
        let additions = hunks.iter().flat_map(|h| &h.lines).filter(|l| l.origin == '+').count();
        let deletions = hunks.iter().flat_map(|h| &h.lines).filter(|l| l.origin == '-').count();
        Self { path, hunks, additions, deletions }
    }
}

/// A hunk within a diff file
#[derive(Clone, Debug)]
pub struct DiffHunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

/// A single line in a diff hunk
#[derive(Clone, Debug)]
pub struct DiffLine {
    pub origin: char,
    pub content: String,
    pub old_lineno: Option<u32>,
    pub new_lineno: Option<u32>,
    /// Byte ranges within `content` that represent intra-line changes (word-level highlight).
    /// Empty means the entire line is changed (no paired line found for comparison).
    pub highlight_ranges: Vec<(usize, usize)>,
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
    pub is_dirty: bool,
}

/// Stash entry information
#[derive(Clone, Debug)]
pub struct StashEntry {
    pub index: usize,
    pub message: String,
    pub time: i64,
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

/// Result of a remote git operation (fetch, push, pull)
#[derive(Debug, Clone)]
pub struct RemoteOpResult {
    pub success: bool,
    #[allow(dead_code)]
    pub output: String,
    pub error: String,
}

/// Full commit information for the detail panel
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct FullCommitInfo {
    pub id: Oid,
    pub short_id: String,
    pub summary: String,
    pub full_message: String,
    pub author_name: String,
    pub author_email: String,
    pub committer_name: String,
    pub committer_email: String,
    pub author_time: i64,
    pub commit_time: i64,
    pub parent_ids: Vec<Oid>,
    pub parent_short_ids: Vec<String>,
}

impl FullCommitInfo {
    pub fn relative_author_time(&self) -> String {
        format_relative_time(self.author_time)
    }
}

impl GitRepo {
    /// Get the working directory path as a PathBuf
    pub fn working_dir_path(&self) -> Option<PathBuf> {
        self.repo.workdir().map(|p| p.to_path_buf())
    }

    /// Find the default remote name (usually "origin")
    pub fn default_remote(&self) -> Result<String> {
        // Try to find the upstream remote for the current branch
        if let Ok(head) = self.repo.head()
            && let Some(name) = head.shorthand()
                && let Ok(branch) = self.repo.find_branch(name, git2::BranchType::Local)
                    && let Ok(upstream) = branch.upstream()
                        && let Ok(Some(upstream_name)) = upstream.name() {
                            // upstream name is like "origin/main", extract remote part
                            if let Some(remote) = upstream_name.split('/').next() {
                                return Ok(remote.to_string());
                            }
                        }
        // Fallback: try "origin"
        if self.repo.find_remote("origin").is_ok() {
            return Ok("origin".to_string());
        }
        // Fallback: first remote
        let remotes = self.repo.remotes().context("No remotes configured")?;
        remotes.get(0)
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("No remotes configured"))
    }

    /// Checkout a local branch by name
    pub fn checkout_branch(&self, name: &str) -> Result<()> {
        let branch = self.repo.find_branch(name, git2::BranchType::Local)
            .with_context(|| format!("Branch '{}' not found", name))?;
        let reference = branch.get().resolve()
            .context("Failed to resolve branch reference")?;
        let commit = reference.peel_to_commit()
            .context("Failed to peel to commit")?;
        let tree = commit.tree().context("Failed to get tree")?;

        self.repo.checkout_tree(tree.as_object(), Some(
            git2::build::CheckoutBuilder::new().safe()
        )).context("Failed to checkout tree")?;

        let refname = format!("refs/heads/{}", name);
        self.repo.set_head(&refname)
            .with_context(|| format!("Failed to set HEAD to {}", name))?;

        Ok(())
    }

    /// Checkout a remote branch, creating a local tracking branch
    pub fn checkout_remote_branch(&self, remote: &str, branch: &str) -> Result<()> {
        // Check if local branch already exists
        if self.repo.find_branch(branch, git2::BranchType::Local).is_ok() {
            // Just checkout the existing local branch
            return self.checkout_branch(branch);
        }

        // Find the remote branch
        let remote_branch_name = format!("{}/{}", remote, branch);
        let remote_ref = self.repo.find_branch(&remote_branch_name, git2::BranchType::Remote)
            .with_context(|| format!("Remote branch '{}' not found", remote_branch_name))?;
        let commit = remote_ref.get().peel_to_commit()
            .context("Failed to peel remote branch to commit")?;

        // Create local tracking branch
        let mut local_branch = self.repo.branch(branch, &commit, false)
            .with_context(|| format!("Failed to create local branch '{}'", branch))?;

        // Set upstream
        local_branch.set_upstream(Some(&remote_branch_name))
            .context("Failed to set upstream")?;

        // Checkout
        let tree = commit.tree().context("Failed to get tree")?;
        self.repo.checkout_tree(tree.as_object(), Some(
            git2::build::CheckoutBuilder::new().safe()
        )).context("Failed to checkout tree")?;

        let refname = format!("refs/heads/{}", branch);
        self.repo.set_head(&refname)?;

        Ok(())
    }

    /// Delete a local branch (refuses to delete the current branch or one checked out in a worktree)
    pub fn delete_branch(&self, name: &str) -> Result<()> {
        // Check if this branch is the current branch (handle bare-repo failures gracefully)
        if let Ok(current) = self.current_branch() {
            if current == name {
                anyhow::bail!("Cannot delete the currently checked-out branch");
            }
        }
        let mut branch = self.repo.find_branch(name, git2::BranchType::Local)
            .with_context(|| format!("Branch '{}' not found", name))?;
        // Check if the branch is checked out in any worktree
        if branch.is_head() {
            anyhow::bail!("Cannot delete the currently checked-out branch");
        }
        branch.delete()
            .with_context(|| format!("Failed to delete branch '{}'", name))?;
        Ok(())
    }

    /// Reset HEAD to a given commit
    pub fn reset_to_commit(&self, oid: Oid, mode: git2::ResetType) -> Result<()> {
        let commit = self.repo.find_commit(oid)
            .with_context(|| format!("Failed to find commit {}", oid))?;
        self.repo.reset(commit.as_object(), mode, None)
            .with_context(|| format!("Failed to reset to {}", oid))?;
        Ok(())
    }

    /// Create a new branch at a given commit OID
    pub fn create_branch_at(&self, name: &str, oid: Oid) -> Result<()> {
        let commit = self.repo.find_commit(oid)
            .with_context(|| format!("Failed to find commit {}", oid))?;
        self.repo.branch(name, &commit, false)
            .with_context(|| format!("Failed to create branch '{}' at {}", name, oid))?;
        Ok(())
    }

    /// Create a lightweight tag at a given commit OID
    pub fn create_tag(&self, name: &str, oid: Oid) -> Result<()> {
        let commit = self.repo.find_commit(oid)
            .with_context(|| format!("Failed to find commit {}", oid))?;
        self.repo.tag_lightweight(name, commit.as_object(), false)
            .with_context(|| format!("Failed to create tag '{}' at {}", name, oid))?;
        Ok(())
    }

    /// Delete a tag by name
    pub fn delete_tag(&self, name: &str) -> Result<()> {
        self.repo.tag_delete(name)
            .with_context(|| format!("Failed to delete tag '{}'", name))?;
        Ok(())
    }

    /// List all stash entries using git CLI (avoids &mut self requirement of libgit2)
    pub fn stash_list(&self) -> Vec<StashEntry> {
        let workdir = match self.repo.workdir().or_else(|| Some(self.repo.path())) {
            Some(p) => p,
            None => return Vec::new(),
        };
        let output = match std::process::Command::new("git")
            .args(["stash", "list", "--format=%gd%x00%s%x00%ct"])
            .current_dir(workdir)
            .output()
        {
            Ok(o) if o.status.success() => o,
            _ => return Vec::new(),
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.lines().enumerate().filter_map(|(i, line)| {
            let parts: Vec<&str> = line.splitn(3, '\0').collect();
            if parts.len() >= 2 {
                let message = parts[1].to_string();
                let time = parts.get(2).and_then(|t| t.parse::<i64>().ok()).unwrap_or(0);
                Some(StashEntry { index: i, message, time })
            } else {
                None
            }
        }).collect()
    }

    /// Amend the last commit with the current index and a new message
    pub fn amend_commit(&self, message: &str) -> Result<Oid> {
        if self.repo.is_bare() {
            anyhow::bail!("Cannot perform this operation on a bare repository");
        }
        let head = self.repo.head().context("Failed to get HEAD")?;
        let head_commit = head.peel_to_commit().context("Failed to get HEAD commit")?;
        let mut index = self.repo.index().context("Failed to get index")?;
        let tree_oid = index.write_tree().context("Failed to write tree")?;
        let tree = self.repo.find_tree(tree_oid).context("Failed to find tree")?;

        let oid = head_commit.amend(
            Some("HEAD"),
            None,  // keep author
            None,  // keep committer
            None,  // keep encoding
            Some(message),
            Some(&tree),
        ).context("Failed to amend commit")?;

        Ok(oid)
    }

    /// Get the HEAD commit's message split into (subject, body)
    pub fn head_commit_message(&self) -> Option<(String, String)> {
        let head = self.repo.head().ok()?;
        let commit = head.peel_to_commit().ok()?;
        let msg = commit.message()?.to_string();
        let mut lines = msg.splitn(2, '\n');
        let subject = lines.next().unwrap_or("").trim().to_string();
        let body = lines.next().unwrap_or("").trim().to_string();
        Some((subject, body))
    }

    /// Discard working directory changes for a file by checking out from HEAD
    pub fn discard_file(&self, path: &str) -> Result<()> {
        let mut checkout_builder = git2::build::CheckoutBuilder::new();
        checkout_builder.path(path).force();
        self.repo.checkout_head(Some(&mut checkout_builder))
            .with_context(|| format!("Failed to discard changes in {}", path))?;
        Ok(())
    }

    /// Get full commit information for the detail panel
    pub fn full_commit_info(&self, oid: Oid) -> Result<FullCommitInfo> {
        let commit = self.repo.find_commit(oid)
            .with_context(|| format!("Failed to find commit {}", oid))?;

        let author = commit.author();
        let committer = commit.committer();

        let parent_ids: Vec<Oid> = commit.parent_ids().collect();
        let parent_short_ids: Vec<String> = parent_ids.iter()
            .map(|id| id.to_string().get(..7).unwrap_or("").to_string())
            .collect();

        Ok(FullCommitInfo {
            id: commit.id(),
            short_id: commit.id().to_string().get(..7).unwrap_or("").to_string(),
            summary: commit.summary().unwrap_or("").to_string(),
            full_message: commit.message().unwrap_or("").to_string(),
            author_name: author.name().unwrap_or("Unknown").to_string(),
            author_email: author.email().unwrap_or("").to_string(),
            committer_name: committer.name().unwrap_or("Unknown").to_string(),
            committer_email: committer.email().unwrap_or("").to_string(),
            author_time: author.when().seconds(),
            commit_time: committer.when().seconds(),
            parent_ids,
            parent_short_ids,
        })
    }

    /// Get diff for a specific file in a commit
    pub fn diff_file_in_commit(&self, oid: Oid, file_path: &str) -> Result<Vec<DiffFile>> {
        let commit = self.repo.find_commit(oid)
            .context("Failed to find commit")?;
        let tree = commit.tree().context("Failed to get commit tree")?;

        let parent_tree = if commit.parent_count() > 0 {
            let parent = commit.parent(0).context("Failed to get parent commit")?;
            Some(parent.tree().context("Failed to get parent tree")?)
        } else {
            None
        };

        let mut opts = git2::DiffOptions::new();
        opts.pathspec(file_path);

        let diff = self.repo.diff_tree_to_tree(
            parent_tree.as_ref(),
            Some(&tree),
            Some(&mut opts),
        ).context("Failed to compute diff")?;

        Self::parse_diff(&diff)
    }

    /// Stage a single hunk from a working-directory file by building a minimal
    /// unified-diff patch and applying it to the index via `git apply --cached`.
    pub fn stage_hunk(&self, file_path: &str, hunk_index: usize) -> Result<()> {
        let hunks = self.diff_working_file(file_path, false)?;
        let hunk = hunks.get(hunk_index)
            .ok_or_else(|| anyhow::anyhow!("Hunk index {} out of range (file has {} hunks)", hunk_index, hunks.len()))?;

        let patch = Self::build_hunk_patch(file_path, file_path, hunk);
        let workdir = self.repo.workdir()
            .ok_or_else(|| anyhow::anyhow!("No working directory"))?;

        let output = std::process::Command::new("git")
            .args(["apply", "--cached", "--unidiff-zero", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .current_dir(workdir)
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(ref mut stdin) = child.stdin {
                    stdin.write_all(patch.as_bytes())?;
                }
                child.wait_with_output()
            })
            .context("Failed to run git apply")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to stage hunk: {}", stderr);
        }
        Ok(())
    }

    /// Unstage a single hunk from the index by building a reverse patch and applying it.
    pub fn unstage_hunk(&self, file_path: &str, hunk_index: usize) -> Result<()> {
        let hunks = self.diff_working_file(file_path, true)?;
        let hunk = hunks.get(hunk_index)
            .ok_or_else(|| anyhow::anyhow!("Hunk index {} out of range (file has {} hunks)", hunk_index, hunks.len()))?;

        let patch = Self::build_hunk_patch(file_path, file_path, hunk);
        let workdir = self.repo.workdir()
            .ok_or_else(|| anyhow::anyhow!("No working directory"))?;

        let output = std::process::Command::new("git")
            .args(["apply", "--cached", "--reverse", "--unidiff-zero", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .current_dir(workdir)
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(ref mut stdin) = child.stdin {
                    stdin.write_all(patch.as_bytes())?;
                }
                child.wait_with_output()
            })
            .context("Failed to run git apply --reverse")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to unstage hunk: {}", stderr);
        }
        Ok(())
    }

    /// Build a minimal unified-diff patch for a single hunk.
    fn build_hunk_patch(old_path: &str, new_path: &str, hunk: &DiffHunk) -> String {
        let mut patch = String::new();
        patch.push_str(&format!("--- a/{}\n", old_path));
        patch.push_str(&format!("+++ b/{}\n", new_path));
        patch.push_str(&hunk.header);
        if !hunk.header.ends_with('\n') {
            patch.push('\n');
        }
        for line in &hunk.lines {
            patch.push(line.origin);
            patch.push_str(&line.content);
            if !line.content.ends_with('\n') {
                patch.push('\n');
            }
        }
        patch
    }
}

/// Spawn a background thread to run a git CLI command and send the result over a channel.
fn run_git_async(args: Vec<String>, workdir: PathBuf, op_name: &str) -> Receiver<RemoteOpResult> {
    let op_name = op_name.to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = std::process::Command::new("git")
            .args(&args)
            .current_dir(&workdir)
            .output();
        let op_result = match result {
            Ok(output) => RemoteOpResult {
                success: output.status.success(),
                output: String::from_utf8_lossy(&output.stdout).to_string(),
                error: String::from_utf8_lossy(&output.stderr).to_string(),
            },
            Err(e) => RemoteOpResult {
                success: false,
                output: String::new(),
                error: format!("Failed to run git {}: {}", op_name, e),
            },
        };
        let _ = tx.send(op_result);
    });
    rx
}

/// Spawn a background thread to run `git fetch --prune`
pub fn fetch_remote_async(workdir: PathBuf, remote: String) -> Receiver<RemoteOpResult> {
    run_git_async(vec!["fetch".into(), "--prune".into(), remote], workdir, "fetch")
}

/// Spawn a background thread to run `git push`
pub fn push_remote_async(workdir: PathBuf, remote: String, branch: String) -> Receiver<RemoteOpResult> {
    run_git_async(vec!["push".into(), remote, branch], workdir, "push")
}

/// Spawn a background thread to run `git pull`
pub fn pull_remote_async(workdir: PathBuf, remote: String, branch: String) -> Receiver<RemoteOpResult> {
    run_git_async(vec!["pull".into(), remote, branch], workdir, "pull")
}

/// Spawn a background thread to remove a submodule (deinit + rm)
pub fn remove_submodule_async(workdir: PathBuf, name: String) -> Receiver<RemoteOpResult> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // Step 1: deinit
        let deinit = std::process::Command::new("git")
            .args(["submodule", "deinit", "-f", &name])
            .current_dir(&workdir)
            .output();
        match deinit {
            Ok(output) if output.status.success() => {
                // Step 2: rm
                let rm = std::process::Command::new("git")
                    .args(["rm", "-f", &name])
                    .current_dir(&workdir)
                    .output();
                let op_result = match rm {
                    Ok(output) => RemoteOpResult {
                        success: output.status.success(),
                        output: String::from_utf8_lossy(&output.stdout).to_string(),
                        error: String::from_utf8_lossy(&output.stderr).to_string(),
                    },
                    Err(e) => RemoteOpResult {
                        success: false,
                        output: String::new(),
                        error: format!("Failed to run git rm: {}", e),
                    },
                };
                let _ = tx.send(op_result);
            }
            Ok(output) => {
                let _ = tx.send(RemoteOpResult {
                    success: false,
                    output: String::from_utf8_lossy(&output.stdout).to_string(),
                    error: String::from_utf8_lossy(&output.stderr).to_string(),
                });
            }
            Err(e) => {
                let _ = tx.send(RemoteOpResult {
                    success: false,
                    output: String::new(),
                    error: format!("Failed to run git submodule deinit: {}", e),
                });
            }
        }
    });
    rx
}

/// Spawn a background thread to update a submodule
pub fn update_submodule_async(workdir: PathBuf, name: String) -> Receiver<RemoteOpResult> {
    run_git_async(vec!["submodule".into(), "update".into(), "--init".into(), name], workdir, "submodule update")
}

/// Spawn a background thread to remove a worktree
pub fn remove_worktree_async(workdir: PathBuf, name: String) -> Receiver<RemoteOpResult> {
    run_git_async(vec!["worktree".into(), "remove".into(), name], workdir, "worktree remove")
}

/// Spawn a background thread to merge a branch into the current branch
pub fn merge_branch_async(workdir: PathBuf, branch_name: String) -> Receiver<RemoteOpResult> {
    run_git_async(vec!["merge".into(), branch_name], workdir, "merge")
}

/// Spawn a background thread to rebase the current branch onto another branch
pub fn rebase_branch_async(workdir: PathBuf, branch_name: String) -> Receiver<RemoteOpResult> {
    run_git_async(vec!["rebase".into(), branch_name], workdir, "rebase")
}

/// Spawn a background thread to stash all changes
pub fn stash_push_async(workdir: PathBuf) -> Receiver<RemoteOpResult> {
    run_git_async(vec!["stash".into(), "push".into()], workdir, "stash push")
}

/// Spawn a background thread to pop the most recent stash
pub fn stash_pop_async(workdir: PathBuf) -> Receiver<RemoteOpResult> {
    run_git_async(vec!["stash".into(), "pop".into()], workdir, "stash pop")
}

/// Spawn a background thread to apply a stash entry (without removing it)
pub fn stash_apply_async(workdir: PathBuf, index: usize) -> Receiver<RemoteOpResult> {
    run_git_async(vec!["stash".into(), "apply".into(), format!("stash@{{{}}}", index)], workdir, "stash apply")
}

/// Spawn a background thread to drop a stash entry
pub fn stash_drop_async(workdir: PathBuf, index: usize) -> Receiver<RemoteOpResult> {
    run_git_async(vec!["stash".into(), "drop".into(), format!("stash@{{{}}}", index)], workdir, "stash drop")
}

/// Spawn a background thread to pop a stash entry by index
pub fn stash_pop_index_async(workdir: PathBuf, index: usize) -> Receiver<RemoteOpResult> {
    run_git_async(vec!["stash".into(), "pop".into(), format!("stash@{{{}}}", index)], workdir, "stash pop")
}

/// Spawn a background thread to cherry-pick a commit
pub fn cherry_pick_async(workdir: PathBuf, sha: String) -> Receiver<RemoteOpResult> {
    run_git_async(vec!["cherry-pick".into(), sha], workdir, "cherry-pick")
}

/// Spawn a background thread to revert a commit
pub fn revert_commit_async(workdir: PathBuf, sha: String) -> Receiver<RemoteOpResult> {
    run_git_async(vec!["revert".into(), "--no-edit".into(), sha], workdir, "revert")
}
