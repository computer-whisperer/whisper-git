//! Git operations via libgit2 (git2 crate).
//!
//! Provides GitRepo wrapper for repository access, async CLI operations via std::thread,
//! and synthetic commit entries for visualizing dirty worktrees in the commit graph.

mod async_ops;
mod diff;
mod hunk;
mod refs;
mod status;

pub use async_ops::*;
pub use diff::{DiffFile, DiffHunk, DiffLine};
pub use status::{FileStatus, FileStatusKind, WorkingDirStatus, working_dir_status_from_statuses};

use anyhow::{Context, Result};
use git2::{Commit, Oid, Repository, RepositoryState};
use std::cmp::Reverse;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use winit::event_loop::EventLoopProxy;

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

/// Compute a fingerprint of the repository's ref state by opening a FRESH
/// git2::Repository handle (bypassing any cached state), reading HEAD OID +
/// sorted local branch tip OIDs, and hashing them into a u64.
/// Cost: ~0.5ms. Returns 0 on error.
pub fn ref_fingerprint(git_dir: &Path) -> u64 {
    let repo = match Repository::open(git_dir) {
        Ok(r) => r,
        Err(_) => return 0,
    };
    let mut hasher = DefaultHasher::new();
    // Hash HEAD target
    if let Ok(head) = repo.head()
        && let Some(oid) = head.target()
    {
        oid.as_bytes().hash(&mut hasher);
    }
    // Hash sorted local branch tip OIDs
    if let Ok(branches) = repo.branches(Some(git2::BranchType::Local)) {
        let mut oids: Vec<Oid> = branches
            .filter_map(|b| b.ok())
            .filter_map(|(b, _)| b.get().resolve().ok()?.target())
            .collect();
        oids.sort();
        for oid in &oids {
            oid.as_bytes().hash(&mut hasher);
        }
    }
    hasher.finish()
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
        if name == ".git" {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if let Ok(modified) = meta.modified() {
                let ts = modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                newest = Some(newest.map_or(ts, |n: i64| n.max(ts)));
            }
            // One level deep for directories
            if meta.is_dir()
                && let Ok(sub_entries) = fs::read_dir(entry.path())
            {
                for sub in sub_entries.flatten() {
                    if let Ok(sub_meta) = sub.metadata()
                        && let Ok(modified) = sub_meta.modified()
                    {
                        let ts = modified
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64;
                        newest = Some(newest.map_or(ts, |n: i64| n.max(ts)));
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
        if let Some(head) = head_oid
            && let Ok(status) = repo.status()
        {
            let count = status.total_files();
            if count > 0 {
                // Find parent commit time
                let parent_time = commits
                    .iter()
                    .find(|c| c.id == head)
                    .map(|c| c.time)
                    .unwrap_or(0);
                let workdir = repo
                    .workdir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                let (ins, del) = repo.working_tree_diff_stats();
                let mut entry =
                    CommitInfo::synthetic_for_working_dir(head, count, &workdir, parent_time);
                entry.insertions = ins;
                entry.deletions = del;
                synthetics.push(entry);
            }
        }
    } else {
        for wt in worktrees {
            if wt.is_dirty == Some(true) {
                // Find parent commit time
                let parent_time = wt
                    .head_oid
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
        let pos = commits
            .iter()
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
    /// Full body text (all lines after summary), for tooltips.
    pub body_full: Option<String>,
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
        let (body_excerpt, body_full) = match commit.message() {
            Some(msg) => {
                let mut lines = msg.lines();
                lines.next(); // skip summary
                let body_lines: Vec<&str> = lines.collect();
                let excerpt = body_lines
                    .iter()
                    .map(|l| l.trim())
                    .find(|l| !l.is_empty())
                    .map(|l| l.to_string());
                let full = {
                    let text: String = body_lines.join("\n");
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                };
                (excerpt, full)
            }
            None => (None, None),
        };
        Self {
            id: commit.id(),
            short_id: commit.id().to_string().get(..7).unwrap_or("").to_string(),
            summary: commit.summary().unwrap_or("").to_string(),
            body_excerpt,
            body_full,
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

        let count = wt.dirty_file_count.unwrap_or(0);
        let summary = if count == 1 {
            format!("Uncommitted changes ({}): 1 file", wt.name)
        } else {
            format!("Uncommitted changes ({}): {} files", wt.name, count)
        };

        Some(Self {
            id: sentinel,
            short_id: String::new(),
            summary,
            body_excerpt: None,
            body_full: None,
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
    pub fn synthetic_for_working_dir(
        head_oid: Oid,
        dirty_count: usize,
        workdir: &str,
        parent_time: i64,
    ) -> Self {
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
            body_full: None,
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
    pub(crate) repo: Repository,
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

    /// Re-open the underlying git2::Repository from the same git dir path,
    /// clearing all internal caches (refdb, odb). This forces subsequent calls
    /// to read fresh data from disk. Uses Repository::open() (not discover())
    /// so it's fast — no directory walk.
    pub fn reopen(&mut self) -> Result<()> {
        let git_dir = self.repo.path().to_path_buf();
        self.repo = Repository::open(&git_dir)
            .with_context(|| format!("Failed to reopen repository at {:?}", git_dir))?;
        Ok(())
    }

    /// Get the current repository state (e.g. merge/rebase in progress)
    pub fn repo_state(&self) -> RepositoryState {
        self.repo.state()
    }

    /// Clean up an in-progress operation (abort merge, cherry-pick, etc.)
    pub fn cleanup_state(&self) -> Result<()> {
        self.repo
            .cleanup_state()
            .context("Failed to clean up repository state")?;
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

    /// Get the shared common directory (where refs, objects, packed-refs live).
    /// For normal repos this equals git_dir(). For worktrees this is the parent
    /// repo's git dir (e.g. `.bare/` instead of `.bare/worktrees/<name>/`).
    pub fn common_dir(&self) -> &Path {
        self.repo.commondir()
    }

    /// Compute aggregate diff stats for the working tree (staged + unstaged).
    /// Returns (insertions, deletions).
    pub fn working_tree_diff_stats(&self) -> (usize, usize) {
        if self.is_effectively_bare() {
            return (0, 0);
        }
        Self::diff_stats_raw(&self.repo)
    }

    /// Compute diff stats from a raw `git2::Repository` reference.
    /// Usable from background threads that open their own repo handle.
    pub fn diff_stats_raw(repo: &git2::Repository) -> (usize, usize) {
        let mut ins = 0usize;
        let mut del = 0usize;
        // Staged: HEAD-to-index
        if let Ok(head_ref) = repo.head()
            && let Ok(head_tree) = head_ref.peel_to_tree()
            && let Ok(diff) = repo.diff_tree_to_index(Some(&head_tree), None, None)
            && let Ok(stats) = diff.stats()
        {
            ins += stats.insertions();
            del += stats.deletions();
        }
        // Unstaged: index-to-workdir
        if let Ok(diff) = repo.diff_index_to_workdir(None, None)
            && let Ok(stats) = diff.stats()
        {
            ins += stats.insertions();
            del += stats.deletions();
        }
        (ins, del)
    }

    /// Produce the staged diff (HEAD→index) as a unified patch string.
    /// Truncates at `max_bytes` with a marker if the diff is too large.
    pub fn staged_diff_text(&self, max_bytes: usize) -> Result<String> {
        let head = self.repo.head().context("Failed to get HEAD")?;
        let head_tree = head.peel_to_tree().context("Failed to get HEAD tree")?;
        let diff = self
            .repo
            .diff_tree_to_index(Some(&head_tree), Some(&self.repo.index()?), None)
            .context("Failed to compute staged diff")?;

        let mut buf = String::new();
        let mut truncated = false;
        diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
            if truncated {
                return true;
            }
            let origin = line.origin();
            if origin == '+' || origin == '-' || origin == ' ' {
                buf.push(origin);
            }
            if let Ok(content) = std::str::from_utf8(line.content()) {
                buf.push_str(content);
            }
            if buf.len() > max_bytes {
                truncated = true;
                buf.truncate(max_bytes);
                buf.push_str("\n... [diff truncated]");
            }
            true
        })
        .context("Failed to print diff")?;

        Ok(buf)
    }

    /// Get commits for building a graph (includes all branches)
    pub fn commit_graph(&self, max_commits: usize) -> Result<Vec<CommitInfo>> {
        let mut revwalk = self.repo.revwalk().context("Failed to create revwalk")?;

        // Include all branches
        for branch in self.repo.branches(None)? {
            if let Ok((branch, _)) = branch
                && let Ok(reference) = branch.get().resolve()
                && let Some(oid) = reference.target()
            {
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

    /// Find the position of a commit in the topological walk (same ordering as `commit_graph`).
    /// Returns `None` if the commit is not reachable within `max_search` steps.
    pub fn commit_position_in_walk(&self, target: Oid, max_search: usize) -> Result<Option<usize>> {
        let mut revwalk = self.repo.revwalk().context("Failed to create revwalk")?;

        for branch in self.repo.branches(None)? {
            if let Ok((branch, _)) = branch
                && let Ok(reference) = branch.get().resolve()
                && let Some(oid) = reference.target()
            {
                let _ = revwalk.push(oid);
            }
        }

        revwalk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;

        for (i, oid_result) in revwalk.enumerate() {
            if i >= max_search {
                break;
            }
            if let Ok(oid) = oid_result
                && oid == target
            {
                return Ok(Some(i));
            }
        }
        Ok(None)
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
            if let Ok(obj) = self.repo.find_object(oid, None)
                && let Ok(commit) = obj.peel_to_commit()
            {
                tip_oids.push(commit.id());
            }
            true
        });

        for refname in &refnames {
            let Ok(reflog) = self.repo.reflog(refname) else {
                continue;
            };
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
            let Ok(_) = self.repo.find_commit(*oid) else {
                return true;
            };
            // If ANY tip is a descendant of this commit, it's reachable
            !tip_oids
                .iter()
                .any(|tip| self.repo.graph_descendant_of(*tip, *oid).unwrap_or(false))
        });

        // Validate each candidate still exists (not GC'd) and build CommitInfo
        let mut orphans: Vec<CommitInfo> = Vec::new();
        let mut orphan_oids: HashSet<Oid> = HashSet::new();

        for (oid, label) in &candidates {
            let Ok(commit) = self.repo.find_commit(*oid) else {
                continue;
            };
            let mut info = CommitInfo::from_commit(&commit);
            info.is_orphaned = true;
            info.orphan_source = Some(label.clone());
            orphan_oids.insert(*oid);
            orphans.push(info);
        }

        // Chain walk: for each discovered orphan, walk parents up to depth 10
        let mut parent_queue: Vec<(Oid, u8, String)> = orphans
            .iter()
            .flat_map(|o| {
                o.parent_ids
                    .iter()
                    .map(move |&pid| (pid, 1u8, o.short_id.clone()))
            })
            .collect();

        while let Some((pid, depth, source_sha)) = parent_queue.pop() {
            if depth > 10 || known_oids.contains(&pid) || orphan_oids.contains(&pid) || pid == zero
            {
                continue;
            }
            let Ok(commit) = self.repo.find_commit(pid) else {
                continue;
            };
            // Skip if reachable from any branch tip (connects back to main graph)
            if tip_oids
                .iter()
                .any(|tip| self.repo.graph_descendant_of(*tip, pid).unwrap_or(false))
            {
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
        orphans.sort_by_key(|commit| Reverse(commit.time));
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
            commits.sort_by_key(|commit| Reverse(commit.time));
        }

        Ok(commits)
    }

    /// Spawn a background thread to compute diff stats for a list of commit OIDs.
    /// Returns a receiver that yields `(Oid, insertions, deletions)` tuples.
    pub fn compute_diff_stats_async(
        &self,
        oids: Vec<Oid>,
        proxy: EventLoopProxy<()>,
    ) -> Receiver<Vec<(Oid, usize, usize)>> {
        crate::crash_log::breadcrumb(format!("diff_stats_async: {} commits", oids.len()));
        let repo_path = self.repo.path().to_path_buf();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let Ok(repo) = Repository::open(&repo_path) else {
                let _ = tx.send(Vec::new());
                let _ = proxy.send_event(());
                return;
            };
            let mut results = Vec::with_capacity(oids.len());
            for oid in oids {
                let Ok(commit) = repo.find_commit(oid) else {
                    continue;
                };
                let (ins, del) = if let Ok(tree) = commit.tree() {
                    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
                    if let Ok(diff) =
                        repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)
                    {
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
            let _ = proxy.send_event(());
        });
        rx
    }

    /// Get the repository name (basename of workdir or bare repo path)
    pub fn repo_name(&self) -> String {
        // Derive name from common_dir (the shared git repo identity).
        // For normal repos: common_dir = /project/.git/ → parent "project"
        // For bare repos: common_dir = /project/.bare/ → walk up to "project"
        // For linked worktrees: common_dir = /project/Repo.git/ → walk up to "project"
        // This avoids returning a worktree-specific name when opened from a linked worktree.
        let mut dir = self.common_dir();
        loop {
            match dir.file_name().and_then(|n| n.to_str()) {
                Some(name) if !name.starts_with('.') && !name.ends_with(".git") => {
                    return name.to_string();
                }
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
                            && let Ok(Some(name)) = branch.name()
                        {
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
            && let Some(oid) = head.target()
        {
            return Ok(oid);
        }
        // Fallback: first local branch tip
        for branch in self.repo.branches(Some(git2::BranchType::Local))? {
            if let Ok((branch, _)) = branch
                && let Ok(reference) = branch.get().resolve()
                && let Some(oid) = reference.target()
            {
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
            let Ok((branch, _)) = branch_result else {
                continue;
            };
            let Ok(Some(name)) = branch.name() else {
                continue;
            };
            let name = name.to_string();
            let Ok(upstream) = branch.upstream() else {
                continue;
            };
            let Some(local_oid) = branch.get().resolve().ok().and_then(|r| r.target()) else {
                continue;
            };
            let Some(upstream_oid) = upstream.get().resolve().ok().and_then(|r| r.target()) else {
                continue;
            };
            if let Ok((ahead, behind)) = self.repo.graph_ahead_behind(local_oid, upstream_oid)
                && (ahead > 0 || behind > 0)
            {
                result.insert(name, (ahead, behind));
            }
        }
        result
    }

    /// Create a commit with the staged changes
    pub fn commit(&self, message: &str) -> Result<Oid> {
        self.ensure_not_bare()?;
        let mut index = self.repo.index().context("Failed to get index")?;
        let tree_oid = index.write_tree().context("Failed to write tree")?;
        let tree = self
            .repo
            .find_tree(tree_oid)
            .context("Failed to find tree")?;

        let head = self.repo.head().context("Failed to get HEAD")?;
        let parent_commit = head
            .peel_to_commit()
            .context("Failed to get parent commit")?;

        let sig = self.repo.signature().context("Failed to get signature")?;

        let commit_oid = self
            .repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent_commit])
            .context("Failed to create commit")?;

        Ok(commit_oid)
    }

    /// Get submodule metadata (lightweight — no dirty checks).
    ///
    /// Returns names, paths, branch info, and OIDs without opening each
    /// submodule repo or running status scans.  Dirty state is computed
    /// asynchronously via per-submodule background checks.
    pub fn submodules(&self) -> Result<Vec<SubmoduleInfo>> {
        // Submodules are scoped to the current working tree context.
        // For effectively bare repos with no selected worktree context, return none.
        if self.workdir().is_none() {
            return Ok(Vec::new());
        }
        let submodules = self.repo.submodules().context("Failed to get submodules")?;

        let mut infos = Vec::new();
        for sm in submodules {
            let name = sm.name().unwrap_or("unknown").to_string();
            let path = sm.path().to_string_lossy().to_string();

            let head_oid = sm.head_id();
            let index_oid = sm.index_id();
            let workdir_oid = sm.workdir_id();

            // Read branch from the submodule HEAD ref — cheap (no status scan).
            let branch = sm
                .open()
                .ok()
                .and_then(|sub_repo| {
                    sub_repo
                        .head()
                        .ok()
                        .and_then(|h| h.shorthand().map(|s| s.to_string()))
                })
                .unwrap_or_else(|| "unknown".to_string());

            infos.push(SubmoduleInfo {
                name,
                path,
                branch,
                is_dirty: None, // computed asynchronously
                head_oid,
                index_oid,
                workdir_oid,
            });
        }

        Ok(infos)
    }

    /// Get worktree metadata (lightweight — no dirty checks).
    ///
    /// Returns names, paths, branch info, and HEAD OIDs without running
    /// status scans.  Dirty state is computed asynchronously via
    /// per-worktree background checks.
    pub fn worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        let worktrees = self.repo.worktrees().context("Failed to get worktrees")?;

        let mut infos = Vec::new();
        for name in worktrees.iter() {
            if let Some(name) = name
                && let Ok(wt) = self.repo.find_worktree(name)
            {
                let wt_path = wt.path();
                let path = wt_path.to_string_lossy().to_string();

                // Read branch and HEAD OID — cheap (no status scan).
                let (branch, head_oid) = if let Ok(wt_repo) = Repository::open(wt_path) {
                    let head_ref = wt_repo.head().ok();
                    let branch = head_ref
                        .as_ref()
                        .and_then(|h| h.shorthand().map(|s| s.to_string()))
                        .unwrap_or_else(|| "detached".to_string());
                    let head_oid = head_ref.and_then(|h| h.target());
                    (branch, head_oid)
                } else {
                    ("unknown".to_string(), None)
                };

                infos.push(WorktreeInfo {
                    name: name.to_string(),
                    path,
                    branch,
                    head_oid,
                    is_dirty: None,         // computed asynchronously
                    dirty_file_count: None, // computed asynchronously
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
                && let Some(oid) = reference.target()
            {
                let name = branch.name().ok().flatten().unwrap_or("").to_string();
                let is_remote = branch_type == git2::BranchType::Remote;
                let is_head = head_oid == Some(oid);

                let upstream = if !is_remote {
                    branch
                        .upstream()
                        .ok()
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
            Ok(remotes) => remotes
                .iter()
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
            let commit_oid = self
                .repo
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

/// Submodule information
#[derive(Clone, Debug)]
pub struct SubmoduleInfo {
    pub name: String,
    pub path: String,
    pub branch: String,
    /// None = not yet checked (async dirty check pending), Some(bool) = known state
    pub is_dirty: Option<bool>,
    pub head_oid: Option<Oid>,    // what parent's HEAD pins (sm.head_id())
    pub index_oid: Option<Oid>,   // what parent's index currently pins (sm.index_id())
    pub workdir_oid: Option<Oid>, // what submodule workdir currently has checked out
}

/// Per-commit submodule entry: what a commit tree pins for each submodule.
#[derive(Clone, Debug)]
pub struct CommitSubmoduleEntry {
    pub name: String,
    pub path: String,
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
    /// None = not yet checked (async dirty check pending), Some(bool) = known state
    pub is_dirty: Option<bool>,
    pub dirty_file_count: Option<usize>,
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
