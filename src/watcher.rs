use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::git::WorktreeInfo;

/// Debounce interval for working-tree file edits (ms).
const WORKTREE_DEBOUNCE_MS: u64 = 500;

/// Debounce interval for git metadata changes (ms) — faster response for branch/commit updates.
const METADATA_DEBOUNCE_MS: u64 = 150;

/// Hard cap: force-emit even if events keep arriving (ms).
const MAX_DELAY_MS: u64 = 2000;

/// Classifies filesystem events so the consumer can respond proportionally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsChangeKind {
    /// File edits outside .git — lightweight status refresh only.
    WorkingTree,
    /// HEAD, refs, index, packed-refs, etc. — full repo state refresh.
    GitMetadata,
    /// .bare/worktrees/ add/remove — full refresh + update watcher paths.
    WorktreeStructure,
}

impl FsChangeKind {
    /// Higher value = higher priority when coalescing events.
    pub fn priority(self) -> u8 {
        match self {
            FsChangeKind::WorkingTree => 0,
            FsChangeKind::GitMetadata => 1,
            FsChangeKind::WorktreeStructure => 2,
        }
    }

    fn max(self, other: Self) -> Self {
        if other.priority() > self.priority() { other } else { self }
    }
}

/// Watches a repository's working directory and git metadata files for changes,
/// sending a debounced `FsChangeKind` signal when something relevant changes.
pub struct RepoWatcher {
    watcher: RecommendedWatcher,
    /// Git metadata dirs we're actively watching (for diffing when worktrees change).
    watched_worktree_dirs: HashSet<PathBuf>,
    /// Worktree working directories we're actively watching.
    watched_worktree_workdirs: HashSet<PathBuf>,
}

impl RepoWatcher {
    /// Create a new watcher for the given workdir, git dir, and common dir.
    ///
    /// `git_dir` is the worktree-specific git dir (from `repo.path()`).
    /// `common_dir` is the shared git dir (from `repo.commondir()`) where refs,
    /// objects, and packed-refs live. For non-worktree repos these are the same.
    ///
    /// `worktrees` provides the initial list of worktree metadata dirs to watch.
    /// Returns the watcher handle and a receiver that yields `FsChangeKind` after
    /// a debounced period of quiet following filesystem changes.
    pub fn new(
        workdir: &Path,
        git_dir: &Path,
        common_dir: &Path,
        worktrees: &[WorktreeInfo],
    ) -> notify::Result<(Self, Receiver<FsChangeKind>)> {
        let (debounce_tx, debounce_rx) = mpsc::channel::<FsChangeKind>();
        let (raw_tx, raw_rx) = mpsc::channel::<FsChangeKind>();

        // Spawn tiered debounce thread
        spawn_debounce_thread(raw_rx, debounce_tx);

        // Build the event classifier with cloned paths for the closure.
        // Classify against both git_dir and common_dir so events from either
        // are recognised as git metadata changes.
        let git_dir_owned = git_dir.to_path_buf();
        let common_dir_owned = common_dir.to_path_buf();

        let watcher_tx = raw_tx;
        let mut watcher = RecommendedWatcher::new(
            move |res: notify::Result<Event>| {
                if let Ok(event) = res {
                    if let Some(kind) = classify_event(&event, &git_dir_owned, &common_dir_owned) {
                        let _ = watcher_tx.send(kind);
                    }
                }
            },
            Config::default(),
        )?;

        // Watch the working directory recursively for file edits
        watcher.watch(workdir, RecursiveMode::Recursive)?;

        // Watch worktree-specific git dir (HEAD, index for this worktree)
        let _ = watcher.watch(git_dir, RecursiveMode::NonRecursive);

        // Watch shared common dir for refs, packed-refs, config
        // (For non-worktree repos common_dir == git_dir, so this is a no-op duplicate)
        let _ = watcher.watch(common_dir, RecursiveMode::NonRecursive);
        let common_refs = common_dir.join("refs");
        if common_refs.is_dir() {
            let _ = watcher.watch(&common_refs, RecursiveMode::Recursive);
        }

        // Also watch git_dir/refs if it exists and differs from common_dir/refs
        let git_refs = git_dir.join("refs");
        if git_refs != common_refs && git_refs.is_dir() {
            let _ = watcher.watch(&git_refs, RecursiveMode::Recursive);
        }

        // Watch worktrees directory for structural changes (add/remove worktree)
        let worktrees_dir = common_dir.join("worktrees");
        if worktrees_dir.is_dir() {
            let _ = watcher.watch(&worktrees_dir, RecursiveMode::Recursive);
        }

        // Watch each existing worktree's git metadata dir + working directory
        let mut watched_worktree_dirs = HashSet::new();
        let mut watched_worktree_workdirs = HashSet::new();
        for wt in worktrees {
            let wt_meta_dir = common_dir.join("worktrees").join(&wt.name);
            if wt_meta_dir.is_dir() {
                let _ = watcher.watch(&wt_meta_dir, RecursiveMode::NonRecursive);
                watched_worktree_dirs.insert(wt_meta_dir);
            }
            // Also watch the worktree's working directory for file edits
            let wt_work_dir = PathBuf::from(&wt.path);
            if wt_work_dir != workdir && wt_work_dir.is_dir() {
                let _ = watcher.watch(&wt_work_dir, RecursiveMode::Recursive);
                watched_worktree_workdirs.insert(wt_work_dir);
            }
        }

        Ok((
            RepoWatcher {
                watcher,
                watched_worktree_dirs,
                watched_worktree_workdirs,
            },
            debounce_rx,
        ))
    }

    /// Add a path to watch. Ignores errors gracefully (e.g., path doesn't exist).
    pub fn watch_path(&mut self, path: &Path, recursive: bool) {
        let mode = if recursive { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive };
        let _ = self.watcher.watch(path, mode);
    }

    /// Remove a path from watching. Ignores errors gracefully.
    pub fn unwatch_path(&mut self, path: &Path) {
        let _ = self.watcher.unwatch(path);
    }

    /// Diff current watch set against worktree list, adding/removing watches as needed.
    /// `common_dir` is the shared git dir (where worktrees/ metadata lives).
    pub fn update_worktree_watches(&mut self, worktrees: &[WorktreeInfo], common_dir: &Path) {
        // --- Git metadata dirs ---
        let desired: HashSet<PathBuf> = worktrees
            .iter()
            .map(|wt| common_dir.join("worktrees").join(&wt.name))
            .filter(|p| p.is_dir())
            .collect();

        for path in self.watched_worktree_dirs.difference(&desired).cloned().collect::<Vec<_>>() {
            self.unwatch_path(&path);
            self.watched_worktree_dirs.remove(&path);
        }
        for path in desired.difference(&self.watched_worktree_dirs).cloned().collect::<Vec<_>>() {
            self.watch_path(&path, false);
            self.watched_worktree_dirs.insert(path);
        }

        // --- Worktree working directories ---
        let desired_workdirs: HashSet<PathBuf> = worktrees
            .iter()
            .map(|wt| PathBuf::from(&wt.path))
            .filter(|p| p.is_dir())
            .collect();

        for path in self.watched_worktree_workdirs.difference(&desired_workdirs).cloned().collect::<Vec<_>>() {
            self.unwatch_path(&path);
            self.watched_worktree_workdirs.remove(&path);
        }
        for path in desired_workdirs.difference(&self.watched_worktree_workdirs).cloned().collect::<Vec<_>>() {
            self.watch_path(&path, true); // recursive for working dirs
            self.watched_worktree_workdirs.insert(path);
        }
    }
}

/// Classifies a filesystem event into a change kind, or None if irrelevant.
/// Checks against both git_dir (worktree-specific) and common_dir (shared).
fn classify_event(event: &Event, git_dir: &Path, common_dir: &Path) -> Option<FsChangeKind> {
    // Only care about data-changing events
    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {}
        _ => return None,
    }

    let mut result: Option<FsChangeKind> = None;

    for path in &event.paths {
        // Try to classify against common_dir first (has refs, packed-refs, worktrees),
        // then git_dir (has worktree-specific HEAD, index).
        // For non-worktree repos these are the same path.
        if let Some(kind) = classify_git_path(path, common_dir) {
            result = Some(match result {
                Some(k) => k.max(kind),
                None => kind,
            });
            continue;
        }
        if common_dir != git_dir {
            if let Some(kind) = classify_git_path(path, git_dir) {
                result = Some(match result {
                    Some(k) => k.max(kind),
                    None => kind,
                });
                continue;
            }
        }
        // Paths outside both git dirs are working tree changes
        result = Some(match result {
            Some(k) => k.max(FsChangeKind::WorkingTree),
            None => FsChangeKind::WorkingTree,
        });
    }

    result
}

/// Classify a single path relative to a git directory.
/// Returns Some(kind) if the path is inside the git dir, None otherwise.
fn classify_git_path(path: &Path, git_dir: &Path) -> Option<FsChangeKind> {
    if !path.starts_with(git_dir) {
        return None;
    }
    let relative = path.strip_prefix(git_dir).ok()?;
    let rel_str = relative.to_string_lossy();

    // Worktree structure changes (new/removed worktree dirs)
    if rel_str.starts_with("worktrees") {
        let depth = relative.components().count();
        return if depth <= 2 {
            Some(FsChangeKind::WorktreeStructure)
        } else {
            Some(FsChangeKind::GitMetadata)
        };
    }

    // Git metadata files
    if rel_str == "HEAD"
        || rel_str == "index"
        || rel_str.starts_with("refs")
        || rel_str == "MERGE_HEAD"
        || rel_str == "REBASE_HEAD"
        || rel_str == "CHERRY_PICK_HEAD"
        || rel_str == "packed-refs"
        || rel_str == "FETCH_HEAD"
        || rel_str == "ORIG_HEAD"
        || rel_str == "config"
    {
        return Some(FsChangeKind::GitMetadata);
    }

    // Inside git dir but not a tracked metadata file (objects, logs, etc.) — skip
    None
}

/// Spawns a background thread with tiered debounce:
/// - Metadata lane: 150ms debounce (fast response for git ops)
/// - Worktree lane: 500ms debounce (normal for file edits)
/// Both have a 2-second hard cap to prevent indefinite deferral.
fn spawn_debounce_thread(raw_rx: Receiver<FsChangeKind>, out_tx: Sender<FsChangeKind>) {
    std::thread::Builder::new()
        .name("fs-watcher-debounce".into())
        .spawn(move || {
            // Track pending events per lane
            let mut metadata_first: Option<Instant> = None; // first event in current burst
            let mut metadata_last: Option<Instant> = None;  // last event in current burst
            let mut metadata_kind = FsChangeKind::GitMetadata;

            let mut worktree_first: Option<Instant> = None;
            let mut worktree_last: Option<Instant> = None;

            loop {
                // Compute timeout: minimum of both lanes' next fire time
                let now = Instant::now();
                let meta_timeout = lane_timeout(metadata_last, metadata_first, METADATA_DEBOUNCE_MS, MAX_DELAY_MS, now);
                let wt_timeout = lane_timeout(worktree_last, worktree_first, WORKTREE_DEBOUNCE_MS, MAX_DELAY_MS, now);
                let timeout = match (meta_timeout, wt_timeout) {
                    (Some(a), Some(b)) => a.min(b),
                    (Some(a), None) => a,
                    (None, Some(b)) => b,
                    (None, None) => Duration::from_secs(60),
                };

                match raw_rx.recv_timeout(timeout) {
                    Ok(kind) => {
                        let now = Instant::now();
                        match kind {
                            FsChangeKind::WorkingTree => {
                                if worktree_first.is_none() {
                                    worktree_first = Some(now);
                                }
                                worktree_last = Some(now);
                            }
                            FsChangeKind::GitMetadata | FsChangeKind::WorktreeStructure => {
                                if metadata_first.is_none() {
                                    metadata_first = Some(now);
                                }
                                metadata_last = Some(now);
                                metadata_kind = metadata_kind.max(kind);
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // Check which lane(s) should fire
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }

                // Check and fire lanes
                let now = Instant::now();

                if let (Some(first), Some(last)) = (metadata_first, metadata_last) {
                    let debounce_elapsed = now.duration_since(last) >= Duration::from_millis(METADATA_DEBOUNCE_MS);
                    let cap_elapsed = now.duration_since(first) >= Duration::from_millis(MAX_DELAY_MS);
                    if debounce_elapsed || cap_elapsed {
                        if out_tx.send(metadata_kind).is_err() { return; }
                        metadata_first = None;
                        metadata_last = None;
                        metadata_kind = FsChangeKind::GitMetadata;
                    }
                }

                if let (Some(first), Some(last)) = (worktree_first, worktree_last) {
                    let debounce_elapsed = now.duration_since(last) >= Duration::from_millis(WORKTREE_DEBOUNCE_MS);
                    let cap_elapsed = now.duration_since(first) >= Duration::from_millis(MAX_DELAY_MS);
                    if debounce_elapsed || cap_elapsed {
                        if out_tx.send(FsChangeKind::WorkingTree).is_err() { return; }
                        worktree_first = None;
                        worktree_last = None;
                    }
                }
            }
        })
        .expect("Failed to spawn fs-watcher-debounce thread");
}

/// Compute how long until a lane should fire, or None if it has no pending events.
fn lane_timeout(
    last: Option<Instant>,
    first: Option<Instant>,
    debounce_ms: u64,
    max_delay_ms: u64,
    now: Instant,
) -> Option<Duration> {
    let last = last?;
    let first = first?;

    let debounce_remaining = Duration::from_millis(debounce_ms)
        .saturating_sub(now.duration_since(last));
    let cap_remaining = Duration::from_millis(max_delay_ms)
        .saturating_sub(now.duration_since(first));

    Some(debounce_remaining.min(cap_remaining))
}
