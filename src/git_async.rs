//! Off-thread git queries — the async layer for tab refresh.
//!
//! Three spawn helpers, each pure (just take paths/options and a proxy,
//! return a `Receiver<Result>`). The consumer (`WhisperApp::poll_async_ops`)
//! drains the receivers each frame and folds results back into [`RepoTab`]
//! via the reducers in `crate::repo_tab`. No `App` coupling lives here —
//! these helpers can be called from any tab-scoped context.
//!
//! ## Why off-thread
//!
//! Sync libgit2 calls on the main thread stall the Wayland event handle
//! on large repos (Wayland disconnects clients that don't respond within
//! the compositor's timeout). The pre-port engine learned this the hard
//! way; every git query that walks the working tree, the commit graph,
//! or even a worktree's GitRepo open path runs on a worker.
//!
//! ## Two-tier refresh
//!
//! [`spawn_status_refresh`] is the cheap path — working-dir status only,
//! used for working-tree edits. [`spawn_repo_state_refresh`] is the heavy
//! path — full commit walk + branches + tags + worktrees + remotes +
//! ahead/behind + per-worktree GitRepo handles, used for git-metadata
//! changes. Working-tree edits *never* trigger a commit walk.
//!
//! ## Per-entity dirty checks
//!
//! [`spawn_dirty_checks`] fans out one worker per submodule and one per
//! worktree. A slow submodule (e.g. esp-idf with 25 K files) doesn't
//! head-of-line block the parent's pill update — each entity's result
//! arrives independently and is applied via [`DirtyCheckResult::tab_id`]
//! routing. Each worker uses [`StatusOptions::exclude_submodules`] so
//! the per-entity check never recurses into nested submodules.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use git2::Oid;
use winit::event_loop::EventLoopProxy;

use crate::git::{
    BranchTip, CommitInfo, GitRepo, StashEntry, SubmoduleInfo, TagInfo, WorkingDirStatus,
    WorktreeInfo, append_staged_gitlinks, staged_gitlinks, working_dir_status_from_statuses,
};

/// Maximum commits walked per refresh. The legacy used the same cap;
/// `repo_tab` re-imports this so the sync and async paths stay aligned
/// while step 3 (async-init) lands.
pub const MAX_COMMITS: usize = 1000;

// ============================================================================
// Status refresh — cheap, working-dir only
// ============================================================================

/// Result of a working-directory status refresh. Folded back into
/// [`crate::repo_tab::WorktreeView::status`] for the worktree path
/// each status was captured from.
pub struct StatusResult {
    /// Working directory path for [`Self::main_status`].
    pub main_path: Option<PathBuf>,
    /// Main repo working-directory status. `None` for bare repos and
    /// when libgit2 fails to open the path.
    pub main_status: Option<WorkingDirStatus>,
    /// Working directory path for [`Self::staging_status`].
    pub staging_path: Option<PathBuf>,
    /// Staging-context working-directory status — the worktree the
    /// staging well is pointing at, which may differ from the main
    /// repo when the user has switched worktrees.
    pub staging_status: Option<WorkingDirStatus>,
    /// Staging repo state (merge / rebase / cherry-pick in progress).
    pub staging_repo_state: git2::RepositoryState,
    /// Prepared merge commit message (`MERGE_MSG`), captured on the
    /// worker when a merge is in progress. Prefills the commit draft.
    pub staging_merge_msg: Option<String>,
}

/// Spawn a worker that computes working-directory status off-thread.
///
/// Bareness is evaluated *per opened repo*, never as a single flag from
/// the reference repo. A bare reference repo (`*.git`) can still host
/// non-bare linked worktrees whose working trees do need walking — the
/// canonical whisper-git layout is exactly this (bare `whisper-git.git`
/// hosting the `whisper-git.main` worktree). A global `is_bare` would
/// suppress the worktree's status walk and leave the staging well
/// permanently empty even though the worktree is dirty.
pub(crate) fn spawn_status_refresh(
    repo_context_path: PathBuf,
    staging_context_path: Option<PathBuf>,
    proxy: EventLoopProxy<()>,
) -> Receiver<StatusResult> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(compute_status_result(
            &repo_context_path,
            staging_context_path.as_deref(),
        ));
        let _ = proxy.send_event(());
    });
    rx
}

/// Pure core of the status refresh — opens each context path and walks
/// its working dir. Split out from the thread spawn so it's testable
/// without an event loop.
///
/// `GitRepo::status()` does the bareness-aware walk per-repo (empty for
/// an effectively-bare repo, otherwise untracked + recursed +
/// submodule-excluded) — the same path the synchronous engine uses, so
/// async and sync results agree. `workdir()` is likewise `None` for an
/// effectively-bare repo. Each path is resolved independently, so a bare
/// *reference* repo never suppresses a non-bare worktree's status walk
/// (the canonical whisper-git layout: bare `whisper-git.git` + the
/// `whisper-git.main` worktree the staging well points at).
pub(crate) fn compute_status_result(
    repo_context_path: &std::path::Path,
    staging_context_path: Option<&std::path::Path>,
) -> StatusResult {
    let main_repo = GitRepo::open(repo_context_path).ok();
    let main_status = main_repo.as_ref().map(|r| r.status().unwrap_or_default());
    let main_path = main_repo
        .as_ref()
        .and_then(|r| r.workdir().map(|p| p.to_path_buf()));

    let staging_repo = staging_context_path.and_then(|dir| GitRepo::open(dir).ok());
    let (staging_status, staging_repo_state) = match staging_repo.as_ref() {
        Some(r) => (Some(r.status().unwrap_or_default()), r.repo_state()),
        None => (None, git2::RepositoryState::Clean),
    };
    // MERGE_MSG is written by conflicted merges, cherry-picks, and
    // reverts alike — capture it for any of those so the commit draft
    // can carry the conventional message.
    let staging_merge_msg = match staging_repo_state {
        git2::RepositoryState::Merge
        | git2::RepositoryState::CherryPick
        | git2::RepositoryState::CherryPickSequence
        | git2::RepositoryState::Revert
        | git2::RepositoryState::RevertSequence => {
            staging_repo.as_ref().and_then(|r| r.merge_message())
        }
        _ => None,
    };
    let staging_path = staging_repo
        .as_ref()
        .and_then(|r| r.workdir().map(|p| p.to_path_buf()));

    StatusResult {
        main_path,
        main_status,
        staging_path,
        staging_status,
        staging_repo_state,
        staging_merge_msg,
    }
}

// ============================================================================
// Repo-state refresh — heavy, full re-query
// ============================================================================

/// Result of a full repo-state refresh: commits, refs, worktrees,
/// remotes, submodules, stashes, ahead/behind, and pre-opened
/// per-worktree GitRepo handles. Folded back into [`RepoTab`] via the
/// reducer in `crate::repo_tab`.
///
/// Even `Repository::open` for each worktree happens on the worker —
/// on slow filesystems or paths that cross a giant submodule, opening
/// the libgit2 handle alone can stall the main thread.
pub struct RepoStateResult {
    pub commits: Vec<CommitInfo>,
    pub branch_tips: Vec<BranchTip>,
    pub tags: Vec<TagInfo>,
    pub worktrees: Vec<WorktreeInfo>,
    pub remote_names: Vec<String>,
    pub remote_urls: HashMap<String, String>,
    pub is_bare: bool,
    pub stashes: Vec<StashEntry>,
    pub ahead_behind: HashMap<String, (usize, usize)>,
    /// Cheap hash of the contents of `git_dir/refs/`. Compared against
    /// the last-seen fingerprint by the reconciliation timer; a
    /// divergence triggers `repo.reopen()` + a full state refresh.
    pub ref_fingerprint: u64,
    /// Real (non-synthetic) commit OIDs — the input set for the
    /// downstream `compute_diff_stats_async` fanout.
    pub real_oids: Vec<Oid>,
    /// Per-worktree GitRepo handles opened on the worker. Merged into
    /// the per-worktree view cache by the reducer.
    pub worktree_repos: HashMap<PathBuf, GitRepo>,
    /// Per-worktree ref + submodule snapshots, captured on the worker so
    /// the reducer never re-walks refs or submodules on the UI thread.
    /// Keyed by working-dir path (main + each linked worktree).
    pub worktree_snapshots: HashMap<PathBuf, WorktreeSnapshot>,
    /// Errors collected during the refresh. Surface as toasts; do not
    /// blank the existing data on a partial failure.
    pub errors: Vec<String>,
}

/// Branch / HEAD / submodules for one worktree, computed on the worker.
/// Folded into the matching [`crate::repo_tab::WorktreeView`] by the
/// reducer with no further git-fs on the main thread.
pub struct WorktreeSnapshot {
    pub current_branch: String,
    pub head_oid: Option<Oid>,
    pub submodules: Vec<SubmoduleInfo>,
}

impl WorktreeSnapshot {
    /// Capture a snapshot from an open repo handle. All three calls walk
    /// the refdb / `.gitmodules`, so this must run off the main thread.
    fn capture(repo: &GitRepo) -> Self {
        Self {
            current_branch: repo.current_branch().unwrap_or_default(),
            head_oid: repo.head_oid().ok(),
            submodules: repo.submodules().unwrap_or_default(),
        }
    }
}

/// Spawn a worker that recomputes the full repo state off-thread.
/// `show_orphaned_commits` toggles the reflog-walk that brings back
/// commits unreachable from any current ref.
pub(crate) fn spawn_repo_state_refresh(
    repo_context_path: PathBuf,
    show_orphaned_commits: bool,
    proxy: EventLoopProxy<()>,
) -> Receiver<RepoStateResult> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut errors = Vec::new();

        let repo = match GitRepo::open(&repo_context_path) {
            Ok(r) => r,
            Err(e) => {
                errors.push(format!("Failed to open repo: {e}"));
                let _ = tx.send(RepoStateResult {
                    commits: Vec::new(),
                    branch_tips: Vec::new(),
                    tags: Vec::new(),
                    worktrees: Vec::new(),
                    remote_names: Vec::new(),
                    remote_urls: HashMap::new(),
                    is_bare: false,
                    stashes: Vec::new(),
                    ahead_behind: HashMap::new(),
                    ref_fingerprint: 0,
                    real_oids: Vec::new(),
                    worktree_repos: HashMap::new(),
                    worktree_snapshots: HashMap::new(),
                    errors,
                });
                let _ = proxy.send_event(());
                return;
            }
        };

        let graph_result = if show_orphaned_commits {
            repo.commit_graph_with_orphans(MAX_COMMITS)
        } else {
            repo.commit_graph(MAX_COMMITS)
        };
        let commits = match graph_result {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("Failed to load commits: {e}"));
                Vec::new()
            }
        };

        let branch_tips = repo.branch_tips().unwrap_or_else(|e| {
            errors.push(format!("Failed to load branches: {e}"));
            Vec::new()
        });
        let tags = repo.tags().unwrap_or_else(|e| {
            errors.push(format!("Failed to load tags: {e}"));
            Vec::new()
        });

        let worktrees = repo.worktrees().unwrap_or_else(|e| {
            errors.push(format!("Failed to load worktrees: {e}"));
            Vec::new()
        });

        // Open a fresh handle for every worktree on the worker — see the
        // struct doc above for why this can't live on the main thread. The
        // main worktree (the reference repo's workdir, which libgit2 omits
        // from the worktrees list) is opened explicitly so the reducer
        // never has to fall back to a synchronous open for it.
        let mut worktree_repos: HashMap<PathBuf, GitRepo> = HashMap::new();
        if let Some(main_wd) = repo.workdir().map(|p| p.to_path_buf())
            && let Ok(r) = GitRepo::open(&main_wd)
        {
            worktree_repos.insert(main_wd, r);
        }
        for wt in &worktrees {
            let path = PathBuf::from(&wt.path);
            if !worktree_repos.contains_key(&path)
                && let Ok(r) = GitRepo::open(&path)
            {
                worktree_repos.insert(path, r);
            }
        }

        // Branch/HEAD/submodule snapshot per opened handle, captured here
        // so the reducer folds plain values with no UI-thread re-walk.
        let worktree_snapshots: HashMap<PathBuf, WorktreeSnapshot> = worktree_repos
            .iter()
            .map(|(path, r)| (path.clone(), WorktreeSnapshot::capture(r)))
            .collect();

        let remote_names = repo.remote_names();
        let is_bare = repo.is_effectively_bare();
        let remote_urls: HashMap<String, String> = remote_names
            .iter()
            .filter_map(|name| repo.remote_url(name).map(|url| (name.clone(), url)))
            .collect();

        let stashes = repo.stash_list();
        let ahead_behind = repo.all_branches_ahead_behind();
        let ref_fingerprint = crate::git::ref_fingerprint(repo.git_dir());

        let real_oids: Vec<Oid> = commits
            .iter()
            .filter(|c| !c.is_synthetic)
            .map(|c| c.id)
            .collect();

        let _ = tx.send(RepoStateResult {
            commits,
            branch_tips,
            tags,
            worktrees,
            remote_names,
            remote_urls,
            is_bare,
            stashes,
            ahead_behind,
            ref_fingerprint,
            real_oids,
            worktree_repos,
            worktree_snapshots,
            errors,
        });
        let _ = proxy.send_event(());
    });
    rx
}

// ============================================================================
// Diff-pane fetch — hunks for the selected file, off-thread
// ============================================================================

/// Identity of one rendered diff: which worktree / file / source
/// produced it. `epoch` folds in the working-tree status generation
/// (bumped whenever a status refresh lands changed content), so an
/// edit to the selected file re-fetches even though the target is
/// unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffKey {
    /// Working-dir path of the worktree the diff reads from.
    pub worktree: PathBuf,
    /// Repo-relative path of the selected file.
    pub file: String,
    /// `Some` = the file's diff within this commit; `None` = the
    /// working-tree diff.
    pub commit: Option<Oid>,
    /// Working-tree diffs only: diff the staged side (HEAD → index)
    /// rather than the unstaged side.
    pub staged: bool,
    /// Working-tree status generation — see [`DiffKey`] docs.
    pub epoch: u64,
}

impl DiffKey {
    /// Same diff target, ignoring the content generation. Used by the
    /// renderer to keep showing the previous hunks while a re-fetch
    /// for a newer epoch is in flight (stale-while-revalidate),
    /// instead of flashing a loading state on every edit.
    pub fn same_target(&self, other: &Self) -> bool {
        self.worktree == other.worktree
            && self.file == other.file
            && self.commit == other.commit
            && self.staged == other.staged
    }
}

/// Result of an off-thread diff fetch. Carries the key it was
/// computed for so the consumer can tell a current result from a
/// stale one (selection moved while the worker ran).
pub struct DiffFetchResult {
    pub key: DiffKey,
    pub hunks: Vec<crate::git::DiffHunk>,
}

/// Spawn a worker that computes the diff hunks for one file
/// off-thread. Opens its own repo handle — large-file diffs were
/// previously computed synchronously during view construction, which
/// froze the UI thread for the duration (and risked a Wayland
/// disconnect, the exact stall this module exists to prevent).
pub(crate) fn spawn_diff_fetch(
    key: DiffKey,
    proxy: EventLoopProxy<()>,
) -> Receiver<DiffFetchResult> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let hunks = compute_diff_hunks(&key);
        let _ = tx.send(DiffFetchResult { key, hunks });
        let _ = proxy.send_event(());
    });
    rx
}

/// Worker body shared by [`spawn_diff_fetch`] and the synchronous
/// fill used by headless contexts (screenshot mode, dump_bundles)
/// that have no poll loop to drain a receiver.
pub(crate) fn compute_diff_hunks(key: &DiffKey) -> Vec<crate::git::DiffHunk> {
    match GitRepo::open(&key.worktree) {
        Ok(repo) => match key.commit {
            Some(oid) => repo
                .diff_file_in_commit(oid, &key.file)
                .unwrap_or_default()
                .into_iter()
                .flat_map(|f| f.hunks)
                .collect(),
            None => repo
                .diff_working_file(&key.file, key.staged)
                .unwrap_or_default(),
        },
        Err(_) => Vec::new(),
    }
}

// ============================================================================
// Per-entity dirty checks — fan out one worker per submodule / worktree
// ============================================================================

/// Result of a single per-entity dirty check. Carries `tab_id` because
/// these flow over a single global channel (see [`spawn_dirty_checks`])
/// and the consumer needs to route each result back to the originating
/// tab — without conflating tabs whose paths happen to match an old
/// closed-then-reopened tab.
pub enum DirtyCheckResult {
    Submodule {
        tab_id: u64,
        name: String,
        is_dirty: bool,
    },
    Worktree {
        tab_id: u64,
        path: PathBuf,
        /// Count of dirty (non-ignored) entries, computed the same way
        /// as the active view's full status so the pill / synthetic
        /// counts agree with the staging well. `0` means clean.
        dirty_file_count: usize,
        /// Working-tree diff stats (insertions, deletions) computed on
        /// the worker. Feeds the synthetic row's +N/-M chips so the UI
        /// thread never recomputes a diff on apply.
        diff_stats: (usize, usize),
    },
}

impl DirtyCheckResult {
    pub fn tab_id(&self) -> u64 {
        match self {
            DirtyCheckResult::Submodule { tab_id, .. } => *tab_id,
            DirtyCheckResult::Worktree { tab_id, .. } => *tab_id,
        }
    }
}

/// Fan out independent dirty checks across all submodules + worktrees.
/// Each entity gets its own worker, so a 25K-file submodule (esp-idf,
/// linux kernel, etc.) doesn't head-of-line block the parent's pill or
/// any sibling submodule. Results arrive individually through `tx`.
///
/// Returns the number of workers spawned — the caller tracks this in
/// `dirty_checks_in_flight` so it can decide when to back off
/// re-triggering (the receiver keeps draining; the gating is just for
/// avoiding stacking redundant fanouts on every frame).
pub(crate) fn spawn_dirty_checks(
    tab_id: u64,
    submodules: &[SubmoduleInfo],
    worktree_paths: &[PathBuf],
    repo_workdir: Option<PathBuf>,
    tx: &Sender<DirtyCheckResult>,
    proxy: &EventLoopProxy<()>,
) -> usize {
    let mut count = 0;

    for sm in submodules {
        let sm_path = match repo_workdir.as_ref() {
            Some(wd) => wd.join(&sm.path),
            None => continue,
        };
        if !sm_path.is_dir() {
            continue;
        }
        let name = sm.name.clone();
        let tx = tx.clone();
        let proxy = proxy.clone();
        std::thread::spawn(move || {
            let is_dirty = check_dirty(&sm_path);
            let _ = tx.send(DirtyCheckResult::Submodule {
                tab_id,
                name,
                is_dirty,
            });
            let _ = proxy.send_event(());
        });
        count += 1;
    }

    for wt_path in worktree_paths {
        if !wt_path.is_dir() {
            continue;
        }
        let wt_path = wt_path.clone();
        let tx = tx.clone();
        let proxy = proxy.clone();
        std::thread::spawn(move || {
            let (dirty_file_count, diff_stats) = check_worktree_dirty(&wt_path);
            let _ = tx.send(DirtyCheckResult::Worktree {
                tab_id,
                path: wt_path,
                dirty_file_count,
                diff_stats,
            });
            let _ = proxy.send_event(());
        });
        count += 1;
    }

    count
}

/// Cheap dirty check for a single repo path — opens, runs status with
/// submodules excluded, returns whether any non-ignored entry exists.
/// `exclude_submodules` is critical: without it, a submodule's own
/// dirty check would recurse into nested sub-submodules. Staged
/// gitlink changes (hidden by that exclusion) are checked separately
/// so a staged pointer update still reads as dirty.
fn check_dirty(path: &PathBuf) -> bool {
    let Ok(repo) = git2::Repository::open(path) else {
        return false;
    };
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true).exclude_submodules(true);
    repo.statuses(Some(&mut opts)).is_ok_and(|s| {
        s.iter()
            .any(|e| !e.status().intersects(git2::Status::IGNORED))
    }) || !staged_gitlinks(&repo).is_empty()
}

/// Worktree variant — returns the dirty *summary* for one worktree:
/// the non-ignored file count and the working-tree diff stats. This
/// feeds the WT pills + synthetic "uncommitted changes" rows for every
/// worktree (active or not). The active worktree's full file lists come
/// separately from [`spawn_status_refresh`]; keeping the two split means
/// the staging-well list has exactly one writer. The count is computed
/// via `working_dir_status_from_statuses().total_files()` so it agrees
/// with that status path; the diff runs here (on the worker) so the UI
/// thread never recomputes it on apply.
fn check_worktree_dirty(path: &PathBuf) -> (usize, (usize, usize)) {
    let Ok(repo) = git2::Repository::open(path) else {
        return (0, (0, 0));
    };
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .exclude_submodules(true);
    let count = repo
        .statuses(Some(&mut opts))
        .map(|statuses| {
            let mut status = working_dir_status_from_statuses(&statuses);
            append_staged_gitlinks(&repo, &mut status);
            status.total_files()
        })
        .unwrap_or(0);
    let diff_stats = GitRepo::diff_stats_raw(&repo);
    (count, diff_stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unique_temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "whisper-git-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    /// Regression: a bare *reference* repo must not suppress the status
    /// walk of the non-bare worktree the staging well points at. This is
    /// the canonical whisper-git layout (bare `whisper-git.git` +
    /// `whisper-git.main` worktree). Pre-fix, a single `is_bare` flag
    /// derived from the bare reference repo blanked the worktree's status
    /// even while it was dirty — pill showed dirty, staging well empty.
    #[test]
    fn bare_reference_does_not_blank_worktree_status() {
        let root = unique_temp_dir("bare-status");
        let bare = root.join("repo.git");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();

        // Bare reference repo (no working tree of its own).
        git2::Repository::init_bare(&bare).unwrap();
        // A separate non-bare repo standing in for the linked worktree,
        // made dirty with an untracked file. `compute_status_result`
        // resolves the two context paths independently, so this models the
        // reference-bare / staging-non-bare split without worktree linkage.
        git2::Repository::init(&work).unwrap();
        std::fs::write(work.join("dirty.txt"), "uncommitted\n").unwrap();

        let result = compute_status_result(&bare, Some(work.as_path()));

        // Reference (bare) contributes no file list and no key to write to.
        assert_eq!(result.main_path, None);
        assert_eq!(
            result.main_status.map(|s| s.total_files()),
            Some(0),
            "bare reference repo should walk to an empty status"
        );
        // The worktree's real status survives despite the bare reference.
        assert!(
            result.staging_path.is_some(),
            "non-bare worktree should report a workdir key"
        );
        assert_eq!(
            result.staging_status.as_ref().map(|s| s.total_files()),
            Some(1),
            "non-bare worktree must report its dirty file"
        );
        assert_eq!(result.staging_status.map(|s| s.untracked.len()), Some(1));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The status worker must surface an in-progress merge (state +
    /// prepared MERGE_MSG) so the staging well can show the operation
    /// banner and prefill the commit draft.
    #[test]
    fn status_result_carries_merge_state_and_message() {
        let dir = unique_temp_dir("merge-state");
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .expect("run git")
        };
        assert!(run(&["init", "-b", "main"]).status.success());
        assert!(run(&["config", "user.name", "Test"]).status.success());
        assert!(run(&["config", "user.email", "t@e.st"]).status.success());
        std::fs::write(dir.join("f.txt"), "base\n").unwrap();
        assert!(run(&["add", "f.txt"]).status.success());
        assert!(run(&["commit", "-m", "base"]).status.success());
        assert!(run(&["checkout", "-b", "feature"]).status.success());
        std::fs::write(dir.join("f.txt"), "feature\n").unwrap();
        assert!(run(&["commit", "-am", "feature"]).status.success());
        assert!(run(&["checkout", "main"]).status.success());
        std::fs::write(dir.join("f.txt"), "main\n").unwrap();
        assert!(run(&["commit", "-am", "main"]).status.success());
        // Conflicting merge — expected to fail and leave merge state.
        assert!(!run(&["merge", "feature"]).status.success());

        let result = compute_status_result(&dir, Some(dir.as_path()));
        assert_eq!(result.staging_repo_state, git2::RepositoryState::Merge);
        let msg = result.staging_merge_msg.expect("MERGE_MSG captured");
        assert!(msg.contains("feature"), "unexpected MERGE_MSG: {msg}");
        assert_eq!(
            result.staging_status.map(|s| s.conflicted.len()),
            Some(1),
            "conflicted file must be listed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
