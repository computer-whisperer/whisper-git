use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use winit::event_loop::EventLoopProxy;

use crate::git::WorktreeInfo;

/// Result of an off-thread watcher init. The recursive watch on the
/// workdir can stall for hundreds of ms on a large repo (notify walks
/// the directory tree synchronously), so construction runs on a worker
/// — exactly the same Wayland-disconnect risk as the state refresh.
pub type WatcherInitResult = notify::Result<(RepoWatcher, Receiver<FsChangeKind>)>;

/// Spawn a worker that constructs a `RepoWatcher` off-thread. The
/// receiver yields exactly one result and then closes. Construction
/// arguments mirror [`RepoWatcher::new`]; see that constructor's
/// docstring for the role of each path argument.
pub fn spawn_init(
    workdir: Option<PathBuf>,
    git_dir: PathBuf,
    common_dir: PathBuf,
    worktrees: Vec<WorktreeInfo>,
    submodule_paths: Vec<PathBuf>,
    proxy: EventLoopProxy<()>,
) -> Receiver<WatcherInitResult> {
    let (tx, rx) = mpsc::channel();
    let proxy_for_send = proxy.clone();
    std::thread::spawn(move || {
        let result = RepoWatcher::new(
            workdir.as_deref(),
            &git_dir,
            &common_dir,
            &worktrees,
            &submodule_paths,
            proxy,
        );
        let _ = tx.send(result);
        let _ = proxy_for_send.send_event(());
    });
    rx
}

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
        if other.priority() > self.priority() {
            other
        } else {
            self
        }
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
    /// Parent directories of linked worktrees. Watching these catches
    /// removal/rename of an external worktree root; a watch on the root
    /// itself may only surface child deletes as ordinary file edits.
    watched_worktree_parent_dirs: HashSet<PathBuf>,
    /// Linked worktree root paths shared with the classifier closure so
    /// delete-self / parent delete events become structural refreshes.
    worktree_roots: Arc<Mutex<Vec<PathBuf>>>,
    /// Submodule workdirs to drop events for. Shared with the watcher's
    /// classifier closure via Arc; `update_submodule_paths` swaps the
    /// inner Vec so submodules added mid-session start being excluded
    /// without rebuilding the watcher.
    submodule_paths: Arc<Mutex<Vec<PathBuf>>>,
}

impl RepoWatcher {
    /// Create a new watcher for the given workdir, git dir, and common dir.
    ///
    /// `workdir` is `None` for a bare/common repo that owns linked
    /// worktrees but has no working directory of its own.
    ///
    /// `git_dir` is the worktree-specific git dir (from `repo.path()`).
    /// `common_dir` is the shared git dir (from `repo.commondir()`) where refs,
    /// objects, and packed-refs live. For non-worktree repos these are the same.
    ///
    /// `worktrees` provides the initial list of worktree metadata dirs to watch.
    /// Returns the watcher handle and a receiver that yields `FsChangeKind` after
    /// a debounced period of quiet following filesystem changes.
    /// `submodule_paths` are absolute paths to submodule workdirs.  Events
    /// inside these directories are silently dropped — the parent repo's status
    /// (with `exclude_submodules`) is unaffected, and submodule dirty state is
    /// checked independently via the per-entity dirty check system.
    pub fn new(
        workdir: Option<&Path>,
        git_dir: &Path,
        common_dir: &Path,
        worktrees: &[WorktreeInfo],
        submodule_paths: &[PathBuf],
        proxy: EventLoopProxy<()>,
    ) -> notify::Result<(Self, Receiver<FsChangeKind>)> {
        let (debounce_tx, debounce_rx) = mpsc::channel::<FsChangeKind>();
        let (raw_tx, raw_rx) = mpsc::channel::<FsChangeKind>();

        // Spawn tiered debounce thread
        spawn_debounce_thread(raw_rx, debounce_tx, proxy);

        // Build the event classifier with cloned paths for the closure.
        // Classify against both git_dir and common_dir so events from either
        // are recognised as git metadata changes.
        let workdir_owned = workdir.map(Path::to_path_buf);
        let git_dir_owned = git_dir.to_path_buf();
        let common_dir_owned = common_dir.to_path_buf();
        let submodule_paths_shared: Arc<Mutex<Vec<PathBuf>>> =
            Arc::new(Mutex::new(submodule_paths.to_vec()));
        let submodule_paths_for_closure = Arc::clone(&submodule_paths_shared);
        let worktree_roots_shared: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(
            worktrees.iter().map(|wt| PathBuf::from(&wt.path)).collect(),
        ));
        let worktree_roots_for_closure = Arc::clone(&worktree_roots_shared);

        let watcher_tx = raw_tx;
        let mut watcher = RecommendedWatcher::new(
            move |res: notify::Result<Event>| {
                match res {
                    Ok(event) => {
                        let guard = submodule_paths_for_closure
                            .lock()
                            .expect("submodule_paths mutex poisoned");
                        let roots = worktree_roots_for_closure
                            .lock()
                            .expect("worktree_roots mutex poisoned");
                        if let Some(kind) = classify_event(
                            &event,
                            workdir_owned.as_deref(),
                            &git_dir_owned,
                            &common_dir_owned,
                            &guard,
                            &roots,
                        ) {
                            // Drop the lock before sending so a slow
                            // consumer doesn't extend the critical section.
                            drop(guard);
                            drop(roots);
                            let _ = watcher_tx.send(kind);
                        }
                    }
                    Err(_e) => {
                        // Queue overflow or other error — trigger full refresh as safety net
                        let _ = watcher_tx.send(FsChangeKind::GitMetadata);
                    }
                }
            },
            Config::default(),
        )?;

        // Watch the main working directory recursively for file edits,
        // when this repo has one. Bare/common repos can still be
        // useful through linked worktrees; those are watched below.
        if let Some(workdir) = workdir {
            watcher.watch(workdir, RecursiveMode::Recursive)?;
        }

        // Watch worktree-specific git dir (HEAD, index for this worktree)
        if let Err(e) = watcher.watch(git_dir, RecursiveMode::NonRecursive) {
            eprintln!("watcher: failed to watch git_dir {:?}: {e}", git_dir);
        }

        // Watch shared common dir for refs, packed-refs, config
        // (For non-worktree repos common_dir == git_dir, so this is a no-op duplicate)
        if let Err(e) = watcher.watch(common_dir, RecursiveMode::NonRecursive) {
            // Ignore duplicate-watch errors (common_dir == git_dir for non-worktree repos)
            if !format!("{e}").contains("already") {
                eprintln!("watcher: failed to watch common_dir {:?}: {e}", common_dir);
            }
        }
        let common_refs = common_dir.join("refs");
        if common_refs.is_dir()
            && let Err(e) = watcher.watch(&common_refs, RecursiveMode::Recursive)
        {
            eprintln!("watcher: failed to watch refs {:?}: {e}", common_refs);
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
        let mut watched_worktree_parent_dirs = HashSet::new();
        for wt in worktrees {
            let wt_meta_dir = common_dir.join("worktrees").join(&wt.name);
            if wt_meta_dir.is_dir() {
                let _ = watcher.watch(&wt_meta_dir, RecursiveMode::NonRecursive);
                watched_worktree_dirs.insert(wt_meta_dir);
            }
            // Watch the worktree's working directory for file edits.
            // Initial watcher construction happens off-thread, so using
            // Recursive here gives linked worktrees the same coverage as
            // the main workdir without stalling the event loop.
            let wt_work_dir = PathBuf::from(&wt.path);
            if workdir != Some(wt_work_dir.as_path()) && wt_work_dir.is_dir() {
                let _ = watcher.watch(&wt_work_dir, RecursiveMode::Recursive);
                watched_worktree_workdirs.insert(wt_work_dir.clone());
            }
            if let Some(parent) = wt_work_dir.parent()
                && Some(parent) != workdir
                && parent.is_dir()
            {
                let parent = parent.to_path_buf();
                if watched_worktree_parent_dirs.insert(parent.clone()) {
                    let _ = watcher.watch(&parent, RecursiveMode::NonRecursive);
                }
            }
        }

        Ok((
            RepoWatcher {
                watcher,
                watched_worktree_dirs,
                watched_worktree_workdirs,
                watched_worktree_parent_dirs,
                worktree_roots: worktree_roots_shared,
                submodule_paths: submodule_paths_shared,
            },
            debounce_rx,
        ))
    }

    /// Replace the submodule exclusion list. Called when a state
    /// refresh notices a submodule was added or removed; the closure
    /// picks up the new list on its next event without needing a
    /// fresh watcher (the recursive workdir watch survives, so we
    /// avoid the multi-hundred-ms inotify reinstall).
    pub fn update_submodule_paths(&self, paths: Vec<PathBuf>) {
        if let Ok(mut guard) = self.submodule_paths.lock() {
            *guard = paths;
        }
    }

    /// Add a path to watch. Ignores errors gracefully (e.g., path doesn't exist).
    pub fn watch_path(&mut self, path: &Path, recursive: bool) {
        let mode = if recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        let _ = self.watcher.watch(path, mode);
    }

    /// Remove a path from watching. Ignores errors gracefully.
    pub fn unwatch_path(&mut self, path: &Path) {
        let _ = self.watcher.unwatch(path);
    }

    /// Diff current watch set against worktree list, adding/removing watches as needed.
    /// `common_dir` is the shared git dir (where worktrees/ metadata lives).
    pub fn update_worktree_watches(&mut self, worktrees: &[WorktreeInfo], common_dir: &Path) {
        let roots: Vec<PathBuf> = worktrees.iter().map(|wt| PathBuf::from(&wt.path)).collect();
        if let Ok(mut guard) = self.worktree_roots.lock() {
            *guard = roots.clone();
        }

        // --- Git metadata dirs ---
        let desired: HashSet<PathBuf> = worktrees
            .iter()
            .map(|wt| common_dir.join("worktrees").join(&wt.name))
            .filter(|p| p.is_dir())
            .collect();

        for path in self
            .watched_worktree_dirs
            .difference(&desired)
            .cloned()
            .collect::<Vec<_>>()
        {
            self.unwatch_path(&path);
            self.watched_worktree_dirs.remove(&path);
        }
        for path in desired
            .difference(&self.watched_worktree_dirs)
            .cloned()
            .collect::<Vec<_>>()
        {
            self.watch_path(&path, false);
            self.watched_worktree_dirs.insert(path);
        }

        // --- Worktree working directories ---
        let desired_workdirs: HashSet<PathBuf> = worktrees
            .iter()
            .map(|wt| PathBuf::from(&wt.path))
            .filter(|p| p.is_dir())
            .collect();

        for path in self
            .watched_worktree_workdirs
            .difference(&desired_workdirs)
            .cloned()
            .collect::<Vec<_>>()
        {
            self.unwatch_path(&path);
            self.watched_worktree_workdirs.remove(&path);
        }
        for path in desired_workdirs
            .difference(&self.watched_worktree_workdirs)
            .cloned()
            .collect::<Vec<_>>()
        {
            self.watch_path(&path, true);
            self.watched_worktree_workdirs.insert(path);
        }

        // --- Parent directories of linked worktrees ---
        let desired_parents: HashSet<PathBuf> = roots
            .iter()
            .filter_map(|p| p.parent().map(Path::to_path_buf))
            .filter(|p| p.is_dir())
            .collect();

        for path in self
            .watched_worktree_parent_dirs
            .difference(&desired_parents)
            .cloned()
            .collect::<Vec<_>>()
        {
            self.unwatch_path(&path);
            self.watched_worktree_parent_dirs.remove(&path);
        }
        for path in desired_parents
            .difference(&self.watched_worktree_parent_dirs)
            .cloned()
            .collect::<Vec<_>>()
        {
            self.watch_path(&path, false);
            self.watched_worktree_parent_dirs.insert(path);
        }
    }
}

/// Classifies a filesystem event into a change kind, or None if irrelevant.
/// Checks against both git_dir (worktree-specific) and common_dir (shared).
/// Events inside submodule workdirs are silently dropped (return None).
fn classify_event(
    event: &Event,
    workdir: Option<&Path>,
    git_dir: &Path,
    common_dir: &Path,
    submodule_paths: &[PathBuf],
    worktree_roots: &[PathBuf],
) -> Option<FsChangeKind> {
    // Only care about data-changing events
    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {}
        _ => return None,
    }

    let mut result: Option<FsChangeKind> = None;

    for path in &event.paths {
        // Skip events inside submodule workdirs — these don't affect parent
        // repo status and submodule dirty state is checked independently.
        if submodule_paths.iter().any(|sm| path.starts_with(sm)) {
            continue;
        }
        if is_worktree_root_structure_event(event.kind, path, worktree_roots) {
            result = Some(match result {
                Some(k) => k.max(FsChangeKind::WorktreeStructure),
                None => FsChangeKind::WorktreeStructure,
            });
            continue;
        }
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
        if common_dir != git_dir
            && let Some(kind) = classify_git_path(path, git_dir)
        {
            result = Some(match result {
                Some(k) => k.max(kind),
                None => kind,
            });
            continue;
        }
        // Paths outside both git dirs are working tree changes only
        // when they are inside the main workdir or a known linked
        // worktree root. Parent-directory watches for external
        // worktrees should not turn unrelated siblings into refreshes.
        if workdir.is_some_and(|workdir| path.starts_with(workdir))
            || worktree_roots.iter().any(|root| path.starts_with(root))
        {
            result = Some(match result {
                Some(k) => k.max(FsChangeKind::WorkingTree),
                None => FsChangeKind::WorkingTree,
            });
        }
    }

    result
}

fn is_worktree_root_structure_event(
    kind: EventKind,
    path: &Path,
    worktree_roots: &[PathBuf],
) -> bool {
    if !matches!(kind, EventKind::Create(_) | EventKind::Remove(_)) {
        return false;
    }
    worktree_roots.iter().any(|root| {
        path == root
            || (path.parent() == root.parent() && path.file_name() == root.file_name())
            || (matches!(kind, EventKind::Remove(_)) && path.starts_with(root) && !root.exists())
    })
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
///   Both have a 2-second hard cap to prevent indefinite deferral.
fn spawn_debounce_thread(
    raw_rx: Receiver<FsChangeKind>,
    out_tx: Sender<FsChangeKind>,
    proxy: EventLoopProxy<()>,
) {
    std::thread::Builder::new()
        .name("fs-watcher-debounce".into())
        .spawn(move || {
            // Track pending events per lane
            let mut metadata_first: Option<Instant> = None; // first event in current burst
            let mut metadata_last: Option<Instant> = None; // last event in current burst
            let mut metadata_kind = FsChangeKind::GitMetadata;

            let mut worktree_first: Option<Instant> = None;
            let mut worktree_last: Option<Instant> = None;

            loop {
                // Compute timeout: minimum of both lanes' next fire time
                let now = Instant::now();
                let meta_timeout = lane_timeout(
                    metadata_last,
                    metadata_first,
                    METADATA_DEBOUNCE_MS,
                    MAX_DELAY_MS,
                    now,
                );
                let wt_timeout = lane_timeout(
                    worktree_last,
                    worktree_first,
                    WORKTREE_DEBOUNCE_MS,
                    MAX_DELAY_MS,
                    now,
                );
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
                    let debounce_elapsed =
                        now.duration_since(last) >= Duration::from_millis(METADATA_DEBOUNCE_MS);
                    let cap_elapsed =
                        now.duration_since(first) >= Duration::from_millis(MAX_DELAY_MS);
                    if debounce_elapsed || cap_elapsed {
                        if out_tx.send(metadata_kind).is_err() {
                            return;
                        }
                        let _ = proxy.send_event(());
                        metadata_first = None;
                        metadata_last = None;
                        metadata_kind = FsChangeKind::GitMetadata;
                    }
                }

                if let (Some(first), Some(last)) = (worktree_first, worktree_last) {
                    let debounce_elapsed =
                        now.duration_since(last) >= Duration::from_millis(WORKTREE_DEBOUNCE_MS);
                    let cap_elapsed =
                        now.duration_since(first) >= Duration::from_millis(MAX_DELAY_MS);
                    if debounce_elapsed || cap_elapsed {
                        if out_tx.send(FsChangeKind::WorkingTree).is_err() {
                            return;
                        }
                        let _ = proxy.send_event(());
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

    let debounce_remaining =
        Duration::from_millis(debounce_ms).saturating_sub(now.duration_since(last));
    let cap_remaining =
        Duration::from_millis(max_delay_ms).saturating_sub(now.duration_since(first));

    Some(debounce_remaining.min(cap_remaining))
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, DataChange, ModifyKind, RemoveKind};

    #[test]
    fn worktree_root_delete_is_structural_even_outside_git_dir() {
        let root = PathBuf::from("/__whisper_git_missing_worktree_root__");
        let roots = vec![root.clone()];

        assert!(is_worktree_root_structure_event(
            EventKind::Remove(RemoveKind::Folder),
            &root,
            &roots
        ));
        assert!(is_worktree_root_structure_event(
            EventKind::Remove(RemoveKind::File),
            &root.join("src/main.rs"),
            &roots
        ));
    }

    #[test]
    fn worktree_root_create_is_structural_but_child_edit_is_not() {
        let root = PathBuf::from("/__whisper_git_missing_worktree_root__");
        let roots = vec![root.clone()];

        assert!(is_worktree_root_structure_event(
            EventKind::Create(CreateKind::Folder),
            &root,
            &roots
        ));
        assert!(!is_worktree_root_structure_event(
            EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Content)),
            &root.join("src/main.rs"),
            &roots
        ));
    }

    #[test]
    fn linked_worktree_edit_classifies_without_main_workdir() {
        let root = PathBuf::from("/repo/worktrees/feature");
        let event = Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
            .add_path(root.join("src/lib.rs"));

        assert_eq!(
            classify_event(
                &event,
                None,
                Path::new("/repo/common/worktrees/feature"),
                Path::new("/repo/common"),
                &[],
                &[root],
            ),
            Some(FsChangeKind::WorkingTree)
        );
    }
}
