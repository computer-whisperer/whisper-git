//! Git operations via libgit2 (git2 crate).
//!
//! Provides GitRepo wrapper for repository access, async CLI operations via std::thread,
//! and synthetic commit entries for visualizing dirty worktrees in the commit graph.

use anyhow::{Context, Result};
use git2::{Diff, Repository, RepositoryState, Commit, Oid, Status, StatusOptions};
use std::collections::{HashMap, HashSet};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
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

/// Convert a `RepositoryState` to a human-readable description.
/// Returns `None` for the `Clean` state (no operation in progress).
pub fn repo_state_label(state: RepositoryState) -> Option<&'static str> {
    match state {
        RepositoryState::Clean => None,
        RepositoryState::Merge => Some("MERGE IN PROGRESS"),
        RepositoryState::Revert => Some("REVERT IN PROGRESS"),
        RepositoryState::RevertSequence => Some("REVERT SEQUENCE IN PROGRESS"),
        RepositoryState::CherryPick => Some("CHERRY-PICK IN PROGRESS"),
        RepositoryState::CherryPickSequence => Some("CHERRY-PICK SEQUENCE IN PROGRESS"),
        RepositoryState::Bisect => Some("BISECT IN PROGRESS"),
        RepositoryState::Rebase => Some("REBASE IN PROGRESS"),
        RepositoryState::RebaseInteractive => Some("INTERACTIVE REBASE IN PROGRESS"),
        RepositoryState::RebaseMerge => Some("REBASE-MERGE IN PROGRESS"),
        RepositoryState::ApplyMailbox => Some("APPLY MAILBOX IN PROGRESS"),
        RepositoryState::ApplyMailboxOrRebase => Some("APPLY MAILBOX/REBASE IN PROGRESS"),
    }
}

/// Scan a directory for the most recently modified file and return its mtime
/// as a Unix timestamp. Only checks top-level and one level deep to avoid
/// expensive deep traversals. Skips `.git` directories.
fn newest_mtime_in_dir(dir: &str) -> Option<i64> {
    use std::fs;
    let mut newest: Option<i64> = None;
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == ".git" { continue; }
        if let Ok(meta) = entry.metadata() {
            if let Ok(modified) = meta.modified() {
                let ts = modified.duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default().as_secs() as i64;
                newest = Some(newest.map_or(ts, |n: i64| n.max(ts)));
            }
            // One level deep for directories
            if meta.is_dir() {
                if let Ok(sub_entries) = fs::read_dir(entry.path()) {
                    for sub in sub_entries.flatten() {
                        if let Ok(sub_meta) = sub.metadata() {
                            if let Ok(modified) = sub_meta.modified() {
                                let ts = modified.duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default().as_secs() as i64;
                                newest = Some(newest.map_or(ts, |n: i64| n.max(ts)));
                            }
                        }
                    }
                }
            }
        }
    }
    newest
}

/// Create synthetic "uncommitted changes" entries for dirty worktrees.
/// This is shared between main.rs and messages.rs to avoid the bug where
/// synthetic entries disappear after certain operations.
pub fn create_synthetic_entries(
    repo: &GitRepo,
    worktrees: &[WorktreeInfo],
    commits: &[CommitInfo],
) -> Vec<CommitInfo> {
    let head_oid = repo.head_oid().ok();
    let mut synthetics: Vec<CommitInfo> = Vec::new();

    if worktrees.is_empty() {
        // Single-worktree fallback: use working_dir_status if dirty
        if let Some(head) = head_oid {
            if let Ok(status) = repo.status() {
                let count = status.total_files();
                if count > 0 {
                    // Find parent commit time
                    let parent_time = commits.iter()
                        .find(|c| c.id == head)
                        .map(|c| c.time)
                        .unwrap_or(0);
                    let workdir = repo.workdir()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let (ins, del) = repo.working_tree_diff_stats();
                    let mut entry = CommitInfo::synthetic_for_working_dir(head, count, &workdir, parent_time);
                    entry.insertions = ins;
                    entry.deletions = del;
                    synthetics.push(entry);
                }
            }
        }
    } else {
        for wt in worktrees {
            if wt.is_dirty {
                // Find parent commit time
                let parent_time = wt.head_oid
                    .and_then(|oid| commits.iter().find(|c| c.id == oid))
                    .map(|c| c.time)
                    .unwrap_or(0);
                if let Some(mut synthetic) = CommitInfo::synthetic_for_worktree(wt, parent_time) {
                    // Compute diff stats for this worktree
                    if let Ok(wt_repo) = GitRepo::open(&wt.path) {
                        let (ins, del) = wt_repo.working_tree_diff_stats();
                        synthetic.insertions = ins;
                        synthetic.deletions = del;
                    }
                    synthetics.push(synthetic);
                }
            }
        }
    }

    synthetics
}

/// Insert synthetic entries into the commit list sorted by time.
/// Commits are in reverse chronological order (newest first), so each
/// synthetic is inserted at the position where its time >= the next commit's time.
pub fn insert_synthetics_sorted(commits: &mut Vec<CommitInfo>, synthetics: Vec<CommitInfo>) {
    for synthetic in synthetics {
        let pos = commits.iter()
            .position(|c| c.time <= synthetic.time)
            .unwrap_or(commits.len());
        commits.insert(pos, synthetic);
    }
}

/// Information about a single commit
#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub id: Oid,
    pub short_id: String,
    pub summary: String,
    /// First line of the commit body (after the summary), if any.
    pub body_excerpt: Option<String>,
    pub author: String,
    pub author_email: String,
    pub time: i64,
    pub parent_ids: Vec<Oid>,
    /// Number of lines inserted in this commit (0 if not computed)
    pub insertions: usize,
    /// Number of lines deleted in this commit (0 if not computed)
    pub deletions: usize,
    /// True for synthetic "uncommitted changes" rows (not real commits)
    pub is_synthetic: bool,
    /// For synthetic entries: the worktree name this entry represents
    pub synthetic_wt_name: Option<String>,
    /// True for orphaned commits discovered via reflogs (unreachable from any branch tip)
    pub is_orphaned: bool,
    /// Reflog source label for orphaned commits, e.g. "HEAD@{3}: rebase (finish)"
    pub orphan_source: Option<String>,
}

impl CommitInfo {
    fn from_commit(commit: &Commit) -> Self {
        // Extract the first non-empty body line after the summary
        let body_excerpt = commit.message().and_then(|msg| {
            let mut lines = msg.lines();
            lines.next(); // skip summary
            // skip blank separator line(s)
            lines
                .map(|l| l.trim())
                .find(|l| !l.is_empty())
                .map(|l| l.to_string())
        });
        Self {
            id: commit.id(),
            short_id: commit.id().to_string().get(..7).unwrap_or("").to_string(),
            summary: commit.summary().unwrap_or("").to_string(),
            body_excerpt,
            author: commit.author().name().unwrap_or("Unknown").to_string(),
            author_email: commit.author().email().unwrap_or("").to_string(),
            time: commit.time().seconds(),
            parent_ids: commit.parent_ids().collect(),
            insertions: 0,
            deletions: 0,
            is_synthetic: false,
            synthetic_wt_name: None,
            is_orphaned: false,
            orphan_source: None,
        }
    }

    pub fn relative_time(&self) -> String {
        format_relative_time(self.time)
    }

    /// Create a synthetic "uncommitted changes" entry for a dirty worktree.
    /// Uses a deterministic sentinel Oid derived from the worktree name so each
    /// worktree gets a unique, stable fake commit ID.
    /// The timestamp is the most recently modified file in the worktree, bounded
    /// to no earlier than the parent commit's timestamp.
    pub fn synthetic_for_worktree(wt: &WorktreeInfo, parent_time: i64) -> Option<Self> {
        let head = wt.head_oid?;
        // Build a deterministic sentinel Oid from the worktree name using a proper hash
        let mut hasher = DefaultHasher::new();
        wt.name.hash(&mut hasher);
        let hash = hasher.finish(); // u64

        let mut bytes = [0u8; 20];
        bytes[0] = 0xFF; // sentinel prefix
        bytes[1] = 0xFE; // sentinel prefix
        // Spread the 8 hash bytes across bytes 2..10
        for (i, b) in hash.to_le_bytes().iter().enumerate() {
            bytes[2 + i] = *b;
        }
        // bytes 10..20 remain zero
        let sentinel = Oid::from_bytes(&bytes).ok()?;

        let mtime = newest_mtime_in_dir(&wt.path).unwrap_or(parent_time);
        // Bound: never earlier than the parent commit
        let time = mtime.max(parent_time);

        let summary = if wt.dirty_file_count == 1 {
            format!("Uncommitted changes ({}): 1 file", wt.name)
        } else {
            format!("Uncommitted changes ({}): {} files", wt.name, wt.dirty_file_count)
        };

        Some(Self {
            id: sentinel,
            short_id: String::new(),
            summary,
            body_excerpt: None,
            author: String::new(),
            author_email: String::new(),
            time,
            parent_ids: vec![head],
            insertions: 0,
            deletions: 0,
            is_synthetic: true,
            synthetic_wt_name: Some(wt.name.clone()),
            is_orphaned: false,
            orphan_source: None,
        })
    }

    /// Create a synthetic "uncommitted changes" entry for the current working directory
    /// (single-worktree fallback when no linked worktrees exist).
    /// The timestamp is the most recently modified file in the workdir, bounded
    /// to no earlier than the parent commit's timestamp.
    pub fn synthetic_for_working_dir(head_oid: Oid, dirty_count: usize, workdir: &str, parent_time: i64) -> Self {
        let mut bytes = [0u8; 20];
        bytes[0] = 0xFF;
        bytes[1] = 0xFD; // distinct prefix from worktree variant
        let sentinel = Oid::from_bytes(&bytes).unwrap_or(head_oid);

        let mtime = newest_mtime_in_dir(workdir).unwrap_or(parent_time);
        // Bound: never earlier than the parent commit
        let time = mtime.max(parent_time);

        let summary = if dirty_count == 1 {
            "Uncommitted changes: 1 file".to_string()
        } else {
            format!("Uncommitted changes: {} files", dirty_count)
        };

        Self {
            id: sentinel,
            short_id: String::new(),
            summary,
            body_excerpt: None,
            author: String::new(),
            author_email: String::new(),
            time,
            parent_ids: vec![head_oid],
            insertions: 0,
            deletions: 0,
            is_synthetic: true,
            synthetic_wt_name: None, // single-worktree, no specific name
            is_orphaned: false,
            orphan_source: None,
        }
    }
}

/// Repository wrapper for our git operations
pub struct GitRepo {
    repo: Repository,
}

impl GitRepo {
    /// Check if this repo is effectively bare — either truly bare (core.bare=true)
    /// or a bare-style repo with worktrees where core.bare=false but the computed
    /// workdir has no `.git` entry pointing back to the repo.
    pub fn is_effectively_bare(&self) -> bool {
        if self.repo.is_bare() {
            return true;
        }
        match self.repo.workdir() {
            None => true,
            Some(workdir) => !workdir.join(".git").exists(),
        }
    }

    /// Return an error if this is a bare repository (no working directory).
    fn ensure_not_bare(&self) -> Result<()> {
        if self.is_effectively_bare() {
            anyhow::bail!("Cannot perform this operation on a bare repository");
        }
        Ok(())
    }

    /// Open a repository at the given path
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let repo = Repository::discover(path.as_ref())
            .with_context(|| format!("Failed to open repository at {:?}", path.as_ref()))?;
        Ok(Self { repo })
    }

    /// Get the current repository state (e.g. merge/rebase in progress)
    pub fn repo_state(&self) -> RepositoryState {
        self.repo.state()
    }

    /// Clean up an in-progress operation (abort merge, cherry-pick, etc.)
    pub fn cleanup_state(&self) -> Result<()> {
        self.repo.cleanup_state().context("Failed to clean up repository state")?;
        Ok(())
    }

    /// Get the repository's working directory, or None if effectively bare.
    pub fn workdir(&self) -> Option<&Path> {
        if self.is_effectively_bare() {
            None
        } else {
            self.repo.workdir()
        }
    }

    /// Get the repository's git directory (.git or .bare)
    pub fn git_dir(&self) -> &Path {
        self.repo.path()
    }

    /// Compute aggregate diff stats for the working tree (staged + unstaged).
    /// Returns (insertions, deletions).
    pub fn working_tree_diff_stats(&self) -> (usize, usize) {
        if self.is_effectively_bare() {
            return (0, 0);
        }
        let mut ins = 0usize;
        let mut del = 0usize;
        // Staged: HEAD-to-index
        if let Ok(head_ref) = self.repo.head() {
            if let Ok(head_tree) = head_ref.peel_to_tree() {
                if let Ok(diff) = self.repo.diff_tree_to_index(Some(&head_tree), None, None) {
                    if let Ok(stats) = diff.stats() {
                        ins += stats.insertions();
                        del += stats.deletions();
                    }
                }
            }
        }
        // Unstaged: index-to-workdir
        if let Ok(diff) = self.repo.diff_index_to_workdir(None, None) {
            if let Ok(stats) = diff.stats() {
                ins += stats.insertions();
                del += stats.deletions();
            }
        }
        (ins, del)
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

    /// Discover orphaned commits via reflogs that aren't reachable from any branch tip.
    /// Returns commits marked with `is_orphaned = true` and a reflog source label.
    pub fn orphaned_commits_from_reflogs(
        &self,
        known_oids: &HashSet<Oid>,
        max_orphans: usize,
    ) -> Vec<CommitInfo> {
        let zero = Oid::zero();
        // Collect (oid, source_label) from reflogs
        let mut candidates: Vec<(Oid, String)> = Vec::new();
        let mut seen: HashSet<Oid> = HashSet::new();

        // Reflog names to walk: HEAD + all local branches
        let mut refnames: Vec<String> = vec!["HEAD".to_string()];
        if let Ok(branches) = self.repo.branches(Some(git2::BranchType::Local)) {
            for branch in branches.flatten() {
                if let Ok(Some(name)) = branch.0.name() {
                    refnames.push(format!("refs/heads/{}", name));
                }
            }
        }

        // Collect all ref tip OIDs (branches + tags) for reachability checks.
        // A reflog entry that is an ancestor of any ref is NOT orphaned — it's just
        // older than the loaded commit window.
        let mut tip_oids: Vec<Oid> = Vec::new();
        if let Ok(branches) = self.repo.branches(None) {
            for branch in branches.flatten() {
                if let Ok(reference) = branch.0.get().resolve()
                    && let Some(oid) = reference.target()
                {
                    tip_oids.push(oid);
                }
            }
        }
        // Include tag targets (peeled to commit)
        let _ = self.repo.tag_foreach(|oid, _| {
            if let Ok(obj) = self.repo.find_object(oid, None) {
                if let Ok(commit) = obj.peel_to_commit() {
                    tip_oids.push(commit.id());
                }
            }
            true
        });

        for refname in &refnames {
            let Ok(reflog) = self.repo.reflog(refname) else { continue };
            for i in 0..reflog.len() {
                let Some(entry) = reflog.get(i) else { continue };
                let msg = entry.message().unwrap_or("").to_string();
                for oid in [entry.id_new(), entry.id_old()] {
                    if oid == zero || known_oids.contains(&oid) || !seen.insert(oid) {
                        continue;
                    }
                    let label = format!("{}@{{{}}}: {}", refname, i, msg);
                    candidates.push((oid, label));
                }
            }
        }

        // Filter out candidates reachable from any branch tip (not truly orphaned,
        // just beyond the loaded commit window)
        candidates.retain(|(oid, _)| {
            // If the commit doesn't exist anymore, keep it — the CommitInfo step
            // below will skip it via find_commit().
            let Ok(_) = self.repo.find_commit(*oid) else { return true };
            // If ANY tip is a descendant of this commit, it's reachable
            !tip_oids.iter().any(|tip| {
                self.repo.graph_descendant_of(*tip, *oid).unwrap_or(false)
            })
        });

        // Validate each candidate still exists (not GC'd) and build CommitInfo
        let mut orphans: Vec<CommitInfo> = Vec::new();
        let mut orphan_oids: HashSet<Oid> = HashSet::new();

        for (oid, label) in &candidates {
            let Ok(commit) = self.repo.find_commit(*oid) else { continue };
            let mut info = CommitInfo::from_commit(&commit);
            info.is_orphaned = true;
            info.orphan_source = Some(label.clone());
            orphan_oids.insert(*oid);
            orphans.push(info);
        }

        // Chain walk: for each discovered orphan, walk parents up to depth 10
        let mut parent_queue: Vec<(Oid, u8, String)> = orphans.iter()
            .flat_map(|o| o.parent_ids.iter().map(move |&pid| (pid, 1u8, o.short_id.clone())))
            .collect();

        while let Some((pid, depth, source_sha)) = parent_queue.pop() {
            if depth > 10 || known_oids.contains(&pid) || orphan_oids.contains(&pid) || pid == zero {
                continue;
            }
            let Ok(commit) = self.repo.find_commit(pid) else { continue };
            // Skip if reachable from any branch tip (connects back to main graph)
            if tip_oids.iter().any(|tip| self.repo.graph_descendant_of(*tip, pid).unwrap_or(false)) {
                continue;
            }
            let mut info = CommitInfo::from_commit(&commit);
            info.is_orphaned = true;
            info.orphan_source = Some(format!("parent of {}", source_sha));
            orphan_oids.insert(pid);
            // Queue this commit's parents for further walking
            for &grandparent in &info.parent_ids {
                parent_queue.push((grandparent, depth + 1, info.short_id.clone()));
            }
            orphans.push(info);
        }

        // Sort by time descending and cap
        orphans.sort_by(|a, b| b.time.cmp(&a.time));
        orphans.truncate(max_orphans);
        orphans
    }

    /// Get commits for graph including orphaned commits from reflogs.
    pub fn commit_graph_with_orphans(&self, max_commits: usize) -> Result<Vec<CommitInfo>> {
        let mut commits = self.commit_graph(max_commits)?;

        let known_oids: HashSet<Oid> = commits.iter().map(|c| c.id).collect();
        let orphans = self.orphaned_commits_from_reflogs(&known_oids, 100);

        if !orphans.is_empty() {
            commits.extend(orphans);
            // Re-sort by time descending (matching commit_graph's TOPOLOGICAL|TIME order)
            // Stable sort preserves topological order among non-orphans with same timestamp
            commits.sort_by(|a, b| b.time.cmp(&a.time));
        }

        Ok(commits)
    }

    /// Spawn a background thread to compute diff stats for a list of commit OIDs.
    /// Returns a receiver that yields `(Oid, insertions, deletions)` tuples.
    pub fn compute_diff_stats_async(&self, oids: Vec<Oid>) -> Receiver<Vec<(Oid, usize, usize)>> {
        crate::crash_log::breadcrumb(format!("diff_stats_async: {} commits", oids.len()));
        let repo_path = self.repo.path().to_path_buf();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let Ok(repo) = Repository::open(&repo_path) else {
                let _ = tx.send(Vec::new());
                return;
            };
            let mut results = Vec::with_capacity(oids.len());
            for oid in oids {
                let Ok(commit) = repo.find_commit(oid) else { continue };
                let (ins, del) = if let Ok(tree) = commit.tree() {
                    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
                    if let Ok(diff) = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) {
                        if let Ok(stats) = diff.stats() {
                            (stats.insertions(), stats.deletions())
                        } else {
                            (0, 0)
                        }
                    } else {
                        (0, 0)
                    }
                } else {
                    (0, 0)
                };
                results.push((oid, ins, del));
            }
            let _ = tx.send(results);
        });
        rx
    }

    /// Get the repository name (basename of workdir or bare repo path)
    pub fn repo_name(&self) -> String {
        if let Some(workdir) = self.workdir() {
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

    /// Get ahead/behind counts for all local branches that have an upstream.
    /// Returns a map of branch_name -> (ahead, behind).
    pub fn all_branches_ahead_behind(&self) -> HashMap<String, (usize, usize)> {
        let mut result = HashMap::new();
        let Ok(branches) = self.repo.branches(Some(git2::BranchType::Local)) else {
            return result;
        };
        for branch_result in branches {
            let Ok((branch, _)) = branch_result else { continue };
            let Ok(Some(name)) = branch.name() else { continue };
            let name = name.to_string();
            let Ok(upstream) = branch.upstream() else { continue };
            let Some(local_oid) = branch.get().resolve().ok().and_then(|r| r.target()) else { continue };
            let Some(upstream_oid) = upstream.get().resolve().ok().and_then(|r| r.target()) else { continue };
            if let Ok((ahead, behind)) = self.repo.graph_ahead_behind(local_oid, upstream_oid) {
                if ahead > 0 || behind > 0 {
                    result.insert(name, (ahead, behind));
                }
            }
        }
        result
    }

    /// Get working directory status
    pub fn status(&self) -> Result<WorkingDirStatus> {
        if self.is_effectively_bare() {
            return Ok(WorkingDirStatus::default());
        }
        let mut opts = StatusOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true);

        let statuses = self.repo.statuses(Some(&mut opts))
            .context("Failed to get status")?;

        let mut staged = Vec::new();
        let mut unstaged = Vec::new();
        let mut conflicted = Vec::new();

        for entry in statuses.iter() {
            let path = entry.path().unwrap_or("").to_string();
            let status = entry.status();

            // Check for conflicted files first (merge/rebase conflicts)
            if status.contains(Status::CONFLICTED) {
                conflicted.push(FileStatus {
                    path,
                    status: FileStatusKind::Conflicted,
                });
                continue;
            }

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

        Ok(WorkingDirStatus { staged, unstaged, conflicted })
    }

    /// Stage a file
    pub fn stage_file(&self, path: &str) -> Result<()> {
        self.ensure_not_bare()?;
        let mut index = self.repo.index().context("Failed to get index")?;
        index.add_path(Path::new(path)).context("Failed to stage file")?;
        index.write().context("Failed to write index")?;
        Ok(())
    }

    /// Unstage a file
    pub fn unstage_file(&self, path: &str) -> Result<()> {
        self.ensure_not_bare()?;
        let head = self.repo.head().context("Failed to get HEAD")?;
        let head_commit = head.peel_to_commit().context("Failed to get HEAD commit")?;
        self.repo
            .reset_default(Some(head_commit.as_object()), [Path::new(path)])
            .context("Failed to unstage file")?;

        Ok(())
    }

    /// Create a commit with the staged changes
    pub fn commit(&self, message: &str) -> Result<Oid> {
        self.ensure_not_bare()?;
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
        // For bare repos, load submodules from the first worktree that has a .gitmodules
        let wt_repo_holder;
        let repo_ref = if self.workdir().is_none() {
            let wt_names = match self.repo.worktrees() {
                Ok(names) => names,
                Err(_) => return Ok(Vec::new()),
            };
            let mut found = None;
            for name in wt_names.iter().flatten() {
                if let Ok(wt) = self.repo.find_worktree(name) {
                    if let Ok(r) = Repository::open(wt.path()) {
                        if r.workdir().map(|w| w.join(".gitmodules").exists()).unwrap_or(false) {
                            found = Some(r);
                            break;
                        }
                    }
                }
            }
            match found {
                Some(r) => { wt_repo_holder = r; &wt_repo_holder }
                None => return Ok(Vec::new()),
            }
        } else {
            &self.repo
        };
        let submodules = repo_ref.submodules().context("Failed to get submodules")?;

        let mut infos = Vec::new();
        for sm in submodules {
            let name = sm.name().unwrap_or("unknown").to_string();
            let path = sm.path().to_string_lossy().to_string();

            let head_oid = sm.head_id();

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
                head_oid,
            });
        }

        Ok(infos)
    }

    /// Get worktrees
    pub fn worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        let worktrees = self.repo.worktrees().context("Failed to get worktrees")?;

        // Get the current working directory for comparison
        let current_workdir = self.workdir().map(|p| p.to_path_buf());

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

                    // Try to get branch info, HEAD oid, and dirty status
                    let (branch, head_oid, is_dirty, dirty_file_count) = if let Ok(wt_repo) = Repository::open(wt_path) {
                        let head_ref = wt_repo.head().ok();
                        let branch = head_ref
                            .as_ref()
                            .and_then(|h| h.shorthand().map(|s| s.to_string()))
                            .unwrap_or_else(|| "detached".to_string());
                        let head_oid = head_ref.and_then(|h| h.target());

                        let (is_dirty, dirty_file_count) = wt_repo
                            .statuses(None)
                            .map(|statuses| {
                                let count = statuses.iter()
                                    .filter(|entry| !entry.status().intersects(Status::IGNORED))
                                    .count();
                                (count > 0, count)
                            })
                            .unwrap_or((false, 0));

                        (branch, head_oid, is_dirty, dirty_file_count)
                    } else {
                        ("unknown".to_string(), None, false, 0)
                    };

                    infos.push(WorktreeInfo {
                        name: name.to_string(),
                        path,
                        branch,
                        head_oid,
                        is_current,
                        is_dirty,
                        dirty_file_count,
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

                        let upstream = if !is_remote {
                            branch.upstream().ok()
                                .and_then(|u| u.name().ok().flatten().map(|s| s.to_string()))
                        } else {
                            None
                        };

                        tips.push(BranchTip {
                            name,
                            oid,
                            is_remote,
                            is_head,
                            upstream,
                        });
                    }
        }

        Ok(tips)
    }

    /// Returns the list of configured remote names (from git config, not refs)
    pub fn remote_names(&self) -> Vec<String> {
        match self.repo.remotes() {
            Ok(remotes) => remotes.iter()
                .filter_map(|r| r.map(|s| s.to_string()))
                .collect(),
            Err(_) => Vec::new(),
        }
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
    pub conflicted: Vec<FileStatus>,
}

impl WorkingDirStatus {
    pub fn total_files(&self) -> usize {
        self.staged.len() + self.unstaged.len() + self.conflicted.len()
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
    Conflicted,
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
    pub head_oid: Option<Oid>,     // what parent's HEAD pins (sm.head_id())
}

/// Per-commit submodule entry: what a commit tree pins for each submodule.
#[derive(Clone, Debug)]
pub struct CommitSubmoduleEntry {
    pub name: String,
    pub pinned_oid: Oid,
    pub changed: bool,
    pub parent_oid: Option<Oid>,
}

/// Worktree information
#[derive(Clone, Debug)]
pub struct WorktreeInfo {
    pub name: String,
    pub path: String,
    pub branch: String,
    pub head_oid: Option<Oid>,
    pub is_current: bool,
    pub is_dirty: bool,
    pub dirty_file_count: usize,
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
    /// Upstream tracking branch name (e.g. "origin/main"), if any
    pub upstream: Option<String>,
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
    pub error: String,
}

/// Full commit information for the detail panel
#[derive(Clone, Debug)]
pub struct FullCommitInfo {
    pub id: Oid,
    pub short_id: String,
    pub full_message: String,
    pub author_name: String,
    pub author_email: String,
    pub author_time: i64,
    pub parent_short_ids: Vec<String>,
}

impl FullCommitInfo {
    pub fn relative_author_time(&self) -> String {
        format_relative_time(self.author_time)
    }
}

impl GitRepo {
    /// Check if the working directory has any uncommitted changes (staged or unstaged).
    /// Returns the total number of changed files, or 0 for bare repos.
    pub fn uncommitted_change_count(&self) -> usize {
        if self.is_effectively_bare() {
            return 0;
        }
        self.status().map(|s| s.total_files()).unwrap_or(0)
    }

    /// Get a suitable directory for running git CLI commands.
    /// Returns the workdir if available, otherwise falls back to the git dir.
    /// This allows push/fetch/pull to work on bare repos.
    pub fn git_command_dir(&self) -> PathBuf {
        self.workdir()
            .unwrap_or_else(|| self.repo.path())
            .to_path_buf()
    }

    /// Check if git user.name and user.email are configured.
    /// Returns true if `repo.signature()` would succeed (needed for commits).
    pub fn has_user_config(&self) -> bool {
        self.repo.signature().is_ok()
    }

    /// Check if any remotes are configured.
    pub fn has_remotes(&self) -> bool {
        self.repo.remotes().map_or(false, |r| !r.is_empty())
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

    /// Rename a local branch
    pub fn rename_branch(&self, old_name: &str, new_name: &str, force: bool) -> Result<()> {
        let mut branch = self.repo.find_branch(old_name, git2::BranchType::Local)
            .with_context(|| format!("Branch '{}' not found", old_name))?;
        branch.rename(new_name, force)
            .with_context(|| format!("Failed to rename branch '{}' to '{}'", old_name, new_name))?;
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
        let workdir = match self.workdir().or_else(|| Some(self.repo.path())) {
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
        self.ensure_not_bare()?;
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

        let parent_short_ids: Vec<String> = commit.parent_ids()
            .map(|id| id.to_string().get(..7).unwrap_or("").to_string())
            .collect();

        Ok(FullCommitInfo {
            id: commit.id(),
            short_id: commit.id().to_string().get(..7).unwrap_or("").to_string(),
            full_message: commit.message().unwrap_or("").to_string(),
            author_name: author.name().unwrap_or("Unknown").to_string(),
            author_email: author.email().unwrap_or("").to_string(),
            author_time: author.when().seconds(),
            parent_short_ids,
        })
    }

    /// Get submodule entries pinned by a specific commit's tree.
    ///
    /// Walks the tree for entries with `ObjectType::Commit` (git's representation
    /// of submodule pointers). Parses `.gitmodules` blob from the same tree for
    /// name→path mapping. Compares against parent commit's tree to detect changes.
    pub fn submodules_at_commit(&self, oid: Oid) -> Result<Vec<CommitSubmoduleEntry>> {
        let commit = self.repo.find_commit(oid)
            .context("Failed to find commit")?;
        let tree = commit.tree().context("Failed to get commit tree")?;

        // Collect parent tree submodule entries for change detection
        let parent_pins: HashMap<String, Oid> = if commit.parent_count() > 0 {
            if let Ok(parent) = commit.parent(0) {
                if let Ok(parent_tree) = parent.tree() {
                    Self::collect_submodule_pins(&parent_tree)
                } else {
                    HashMap::new()
                }
            } else {
                HashMap::new()
            }
        } else {
            HashMap::new()
        };

        // Parse .gitmodules from this tree for name→path mapping
        let name_map = self.parse_gitmodules_from_tree(&tree);

        // Collect submodule entries from this commit's tree
        let pins = Self::collect_submodule_pins(&tree);

        let mut entries = Vec::new();
        for (path, pinned_oid) in &pins {
            let name = name_map.get(path).cloned().unwrap_or_else(|| path.clone());
            let parent_oid = parent_pins.get(path).copied();
            let changed = match parent_oid {
                Some(parent) => parent != *pinned_oid,
                None => true, // new submodule
            };

            entries.push(CommitSubmoduleEntry {
                name,
                pinned_oid: *pinned_oid,
                changed,
                parent_oid,
            });
        }

        // Sort: changed entries first, then by name
        entries.sort_by(|a, b| b.changed.cmp(&a.changed).then(a.name.cmp(&b.name)));

        Ok(entries)
    }

    /// Collect all submodule pin entries (ObjectType::Commit) from a tree.
    fn collect_submodule_pins(tree: &git2::Tree) -> HashMap<String, Oid> {
        let mut pins = HashMap::new();
        for entry in tree.iter() {
            if entry.kind() == Some(git2::ObjectType::Commit) {
                if let Some(name) = entry.name() {
                    pins.insert(name.to_string(), entry.id());
                }
            }
        }
        pins
    }

    /// Parse `.gitmodules` blob from a tree to build a path→name mapping.
    fn parse_gitmodules_from_tree(&self, tree: &git2::Tree) -> HashMap<String, String> {
        let mut map = HashMap::new();

        let Some(entry) = tree.get_name(".gitmodules") else {
            return map;
        };
        let Ok(obj) = entry.to_object(&self.repo) else {
            return map;
        };
        let Ok(blob) = obj.peel_to_blob() else {
            return map;
        };

        // Simple line-based parser for .gitmodules INI format
        let content = String::from_utf8_lossy(blob.content());
        let mut current_name: Option<String> = None;
        let mut current_path: Option<String> = None;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("[submodule ") {
                // Flush previous entry
                if let (Some(path), Some(name)) = (&current_path, &current_name) {
                    map.insert(path.clone(), name.clone());
                }
                // Parse name from [submodule "name"]
                current_name = trimmed
                    .strip_prefix("[submodule \"")
                    .and_then(|s| s.strip_suffix("\"]"))
                    .map(|s| s.to_string());
                current_path = None;
            } else if let Some(value) = trimmed.strip_prefix("path") {
                let value = value.trim().strip_prefix('=').unwrap_or(value).trim();
                current_path = Some(value.to_string());
            }
        }
        // Flush last entry
        if let (Some(path), Some(name)) = (&current_path, &current_name) {
            map.insert(path.clone(), name.clone());
        }

        map
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
        self.apply_hunk_patch(file_path, hunk_index, false)
    }

    /// Unstage a single hunk from the index by building a reverse patch and applying it.
    pub fn unstage_hunk(&self, file_path: &str, hunk_index: usize) -> Result<()> {
        self.apply_hunk_patch(file_path, hunk_index, true)
    }

    /// Apply a hunk patch to the index. When `reverse` is true the patch is
    /// applied in reverse (unstage); when false it stages the hunk.
    fn apply_hunk_patch(&self, file_path: &str, hunk_index: usize, reverse: bool) -> Result<()> {
        let hunks = self.diff_working_file(file_path, reverse)?;
        let hunk = hunks.get(hunk_index)
            .ok_or_else(|| anyhow::anyhow!("Hunk index {} out of range (file has {} hunks)", hunk_index, hunks.len()))?;

        let patch = Self::build_hunk_patch(file_path, file_path, hunk);
        let workdir = self.workdir()
            .ok_or_else(|| anyhow::anyhow!("No working directory"))?;

        let mut args = vec!["apply", "--cached"];
        if reverse {
            args.push("--reverse");
        }
        args.extend(["--unidiff-zero", "-"]);

        let output = std::process::Command::new("git")
            .args(&args)
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
            .with_context(|| format!("Failed to run git apply{}", if reverse { " --reverse" } else { "" }))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let action = if reverse { "unstage" } else { "stage" };
            anyhow::bail!("Failed to {} hunk: {}", action, stderr);
        }
        Ok(())
    }

    /// Discard a single hunk from the working tree by applying the reverse patch
    /// directly to the working directory (no --cached).
    pub fn discard_hunk(&self, file_path: &str, hunk_index: usize) -> Result<()> {
        let hunks = self.diff_working_file(file_path, false)?;
        let hunk = hunks.get(hunk_index)
            .ok_or_else(|| anyhow::anyhow!("Hunk index {} out of range (file has {} hunks)", hunk_index, hunks.len()))?;

        let patch = Self::build_hunk_patch(file_path, file_path, hunk);
        let workdir = self.workdir()
            .ok_or_else(|| anyhow::anyhow!("No working directory"))?;

        let output = std::process::Command::new("git")
            .args(["apply", "--reverse", "--unidiff-zero", "-"])
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
            .with_context(|| "Failed to run git apply --reverse")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to discard hunk: {}", stderr);
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

    // ---- Remote management ----

    /// Get the URL of a named remote.
    pub fn remote_url(&self, name: &str) -> Option<String> {
        self.repo.find_remote(name)
            .ok()
            .and_then(|r| r.url().map(|u| u.to_string()))
    }

    /// Add a new remote with the given name and URL.
    pub fn add_remote(&self, name: &str, url: &str) -> Result<()> {
        self.repo.remote(name, url)
            .with_context(|| format!("Failed to add remote '{}' with url '{}'", name, url))?;
        Ok(())
    }

    /// Delete a remote by name.
    pub fn delete_remote(&self, name: &str) -> Result<()> {
        self.repo.remote_delete(name)
            .with_context(|| format!("Failed to delete remote '{}'", name))?;
        Ok(())
    }

    /// Rename a remote.
    pub fn rename_remote(&self, old_name: &str, new_name: &str) -> Result<()> {
        let problems = self.repo.remote_rename(old_name, new_name)
            .with_context(|| format!("Failed to rename remote '{}' to '{}'", old_name, new_name))?;
        if !problems.is_empty() {
            let msgs: Vec<&str> = problems.iter().flatten().collect();
            if !msgs.is_empty() {
                eprintln!("Remote rename warnings: {:?}", msgs);
            }
        }
        Ok(())
    }

    /// Change the URL of an existing remote.
    pub fn set_remote_url(&self, name: &str, url: &str) -> Result<()> {
        self.repo.remote_set_url(name, url)
            .with_context(|| format!("Failed to set URL for remote '{}'", name))?;
        Ok(())
    }
}

/// Spawn a background thread to run a git CLI command and send the result over a channel.
fn run_git_async(args: Vec<String>, workdir: PathBuf, op_name: &str) -> Receiver<RemoteOpResult> {
    crate::crash_log::breadcrumb(format!("git_async: {op_name} args={args:?}"));
    let op_name = op_name.to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = std::process::Command::new("git")
            .args(&args)
            .current_dir(&workdir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output();
        let op_result = match result {
            Ok(output) => RemoteOpResult {
                success: output.status.success(),
                error: String::from_utf8_lossy(&output.stderr).to_string(),
            },
            Err(e) => RemoteOpResult {
                success: false,
                error: format!("Failed to run git {}: {}", op_name, e),
            },
        };
        crate::crash_log::breadcrumb(format!("git_async done: {op_name} success={}", op_result.success));
        let _ = tx.send(op_result);
    });
    rx
}

/// Define an async git wrapper that delegates to `run_git_async`.
///
/// Each invocation generates a `pub fn $name(workdir: PathBuf, ...) -> Receiver<RemoteOpResult>`
/// that constructs the arg vector and calls `run_git_async`.
///
/// Syntax:
///   `fn_name(param: Type, ...) => [arg_expr, ...], "op_name";`
macro_rules! define_async_git_op {
    ($(
        $(#[doc = $doc:expr])*
        $name:ident( $($param:ident : $pty:ty),* ) => [ $($arg:expr),+ $(,)? ], $op:expr;
    )*) => {
        $(
            $(#[doc = $doc])*
            pub fn $name(workdir: PathBuf, $($param: $pty),*) -> Receiver<RemoteOpResult> {
                run_git_async(vec![$($arg.into()),+], workdir, $op)
            }
        )*
    };
}

define_async_git_op! {
    /// Spawn a background thread to run `git fetch --prune`
    fetch_remote_async(remote: String) =>
        ["fetch", "--prune", remote], "fetch";

    /// Spawn a background thread to run `git fetch --all --prune`
    fetch_all_async() =>
        ["fetch", "--all", "--prune"], "fetch --all";

    /// Spawn a background thread to run `git push`
    push_remote_async(remote: String, branch: String) =>
        ["push", remote, branch], "push";

    /// Spawn a background thread to run `git push --force-with-lease`
    push_force_async(remote: String, branch: String) =>
        ["push", "--force-with-lease", remote, branch], "push";

    /// Spawn a background thread to run `git push` with a refspec (local:remote format)
    push_refspec_async(remote: String, refspec: String) =>
        ["push", remote, refspec], "push";

    /// Spawn a background thread to run `git push --force-with-lease` with a refspec
    push_force_refspec_async(remote: String, refspec: String) =>
        ["push", "--force-with-lease", remote, refspec], "push";

    /// Spawn a background thread to run `git pull`
    pull_remote_async(remote: String, branch: String) =>
        ["pull", remote, branch], "pull";

    /// Spawn a background thread to run `git pull --rebase`
    pull_rebase_async(remote: String, branch: String) =>
        ["pull", "--rebase", remote, branch], "pull --rebase";

    /// Spawn a background thread to update a submodule
    update_submodule_async(name: String) =>
        ["submodule", "update", "--init", name], "submodule update";

    /// Spawn a background thread to create a worktree for a branch
    create_worktree_async(path: String, branch: String) =>
        ["worktree", "add", path, branch], "worktree add";

    /// Spawn a background thread to create a detached worktree at a commit
    create_worktree_detached_async(path: String, commitish: String) =>
        ["worktree", "add", "--detach", path, commitish], "worktree add";

    /// Spawn a background thread to remove a worktree
    remove_worktree_async(name: String) =>
        ["worktree", "remove", "--force", name], "worktree remove";

    /// Spawn a background thread to delete a branch on the remote
    delete_remote_branch_async(remote: String, branch: String) =>
        ["push", remote, "--delete", branch], "delete remote branch";

    /// Spawn a background thread to merge a branch into the current branch
    merge_branch_async(branch_name: String) =>
        ["merge", branch_name], "merge";

    /// Spawn a background thread to merge with --no-ff (always create merge commit)
    merge_noff_async(branch_name: String, message: String) =>
        ["merge", "--no-ff", "-m", message, branch_name], "merge --no-ff";

    /// Spawn a background thread to merge with --ff-only (fail if not fast-forwardable)
    merge_ffonly_async(branch_name: String) =>
        ["merge", "--ff-only", branch_name], "merge --ff-only";

    /// Spawn a background thread to merge with --squash (stage changes, don't auto-commit)
    merge_squash_async(branch_name: String) =>
        ["merge", "--squash", branch_name], "merge --squash";

}

/// Spawn a background thread to rebase with options (--autostash, --rebase-merges)
pub fn rebase_with_options_async(
    workdir: PathBuf, branch: String, autostash: bool, rebase_merges: bool,
) -> Receiver<RemoteOpResult> {
    let mut args: Vec<String> = vec!["rebase".into()];
    if autostash { args.push("--autostash".into()); }
    if rebase_merges { args.push("--rebase-merges".into()); }
    args.push(branch);
    run_git_async(args, workdir, "rebase")
}

define_async_git_op! {
    /// Spawn a background thread to stash all changes
    stash_push_async() =>
        ["stash", "push"], "stash push";

    /// Spawn a background thread to pop the most recent stash
    stash_pop_async() =>
        ["stash", "pop"], "stash pop";

    /// Spawn a background thread to cherry-pick a commit
    cherry_pick_async(sha: String) =>
        ["cherry-pick", sha], "cherry-pick";

    /// Spawn a background thread to revert a commit
    revert_commit_async(sha: String) =>
        ["revert", "--no-edit", sha], "revert";
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
                        error: String::from_utf8_lossy(&output.stderr).to_string(),
                    },
                    Err(e) => RemoteOpResult {
                        success: false,
                        error: format!("Failed to run git rm: {}", e),
                    },
                };
                let _ = tx.send(op_result);
            }
            Ok(output) => {
                let _ = tx.send(RemoteOpResult {
                    success: false,
                    error: String::from_utf8_lossy(&output.stderr).to_string(),
                });
            }
            Err(e) => {
                let _ = tx.send(RemoteOpResult {
                    success: false,
                    error: format!("Failed to run git submodule deinit: {}", e),
                });
            }
        }
    });
    rx
}

/// Classify a git CLI stderr message into a user-friendly error string.
/// Returns `(friendly_message, is_rejected)` where `is_rejected` indicates
/// the remote rejected the push (e.g. non-fast-forward).
pub fn classify_git_error(op: &str, stderr: &str) -> (String, bool) {
    let lower = stderr.to_lowercase();
    let is_rejected = lower.contains("rejected") || lower.contains("non-fast-forward");

    let friendly = if lower.contains("terminal prompts disabled") || lower.contains("could not read username") {
        format!("{} failed: Authentication required. Configure SSH keys or a credential helper.", op)
    } else if lower.contains("permission denied") {
        format!("{} failed: Permission denied. Check your SSH key or access token.", op)
    } else if lower.contains("could not read password") {
        format!("{} failed: Password required. Set up a credential helper (git config credential.helper cache).", op)
    } else if lower.contains("host key verification failed") {
        format!("{} failed: SSH host key not trusted. Run ssh-keyscan to add the host.", op)
    } else if lower.contains("repository not found") || lower.contains("404") {
        format!("{} failed: Repository not found. Check the remote URL.", op)
    } else if lower.contains("connection refused") || lower.contains("could not resolve") {
        format!("{} failed: Cannot connect to remote. Check your network and remote URL.", op)
    } else if is_rejected {
        format!("{} rejected: Remote has new commits. Pull first, or use Force Push.", op)
    } else {
        // Show up to 3 lines of the error for context
        let error_summary: String = stderr.lines().take(3).collect::<Vec<_>>().join("\n");
        if error_summary.is_empty() {
            format!("{} failed: unknown error", op)
        } else {
            format!("{} failed: {}", op, error_summary)
        }
    };

    (friendly, is_rejected)
}
