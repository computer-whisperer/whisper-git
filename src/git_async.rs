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
    WorktreeInfo, working_dir_status_from_statuses,
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
}

/// Spawn a worker that computes working-directory status off-thread.
/// `is_bare` short-circuits the status walk for bare repos that have
/// no working tree at all.
pub(crate) fn spawn_status_refresh(
    repo_context_path: PathBuf,
    staging_context_path: Option<PathBuf>,
    is_bare: bool,
    proxy: EventLoopProxy<()>,
) -> Receiver<StatusResult> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let main_repo = git2::Repository::open(&repo_context_path).ok();

        let main_status = if !is_bare {
            main_repo.as_ref().and_then(|repo| {
                let mut opts = git2::StatusOptions::new();
                // exclude_submodules is the load-bearing flag here: without
                // it, a parent status walk on a repo with giant submodules
                // (esp-idf, 25K files) takes seconds and stalls the next
                // refresh. Submodule dirty state is checked separately by
                // `spawn_dirty_checks`.
                opts.include_untracked(true)
                    .recurse_untracked_dirs(true)
                    .exclude_submodules(true);
                let statuses = repo.statuses(Some(&mut opts)).ok()?;
                Some(working_dir_status_from_statuses(&statuses))
            })
        } else {
            Some(WorkingDirStatus::default())
        };

        let staging_repo = staging_context_path
            .as_ref()
            .and_then(|dir| git2::Repository::open(dir).ok());

        let (staging_status, staging_repo_state) = match staging_repo.as_ref() {
            Some(repo) => {
                let state = repo.state();
                let status = if !is_bare {
                    let mut opts = git2::StatusOptions::new();
                    opts.include_untracked(true)
                        .recurse_untracked_dirs(true)
                        .exclude_submodules(true);
                    repo.statuses(Some(&mut opts))
                        .ok()
                        .map(|s| working_dir_status_from_statuses(&s))
                } else {
                    Some(WorkingDirStatus::default())
                };
                (status, state)
            }
            None => (None, git2::RepositoryState::Clean),
        };

        let main_path = main_repo
            .as_ref()
            .and_then(|repo| repo.workdir().map(|p| p.to_path_buf()));
        let staging_path = staging_repo
            .as_ref()
            .and_then(|repo| repo.workdir().map(|p| p.to_path_buf()));

        let _ = tx.send(StatusResult {
            main_path,
            main_status,
            staging_path,
            staging_status,
            staging_repo_state,
        });
        let _ = proxy.send_event(());
    });
    rx
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
    pub current_branch: String,
    pub head_oid: Option<Oid>,
    pub worktrees: Vec<WorktreeInfo>,
    pub remote_names: Vec<String>,
    pub remote_urls: HashMap<String, String>,
    pub is_bare: bool,
    pub submodules: Vec<SubmoduleInfo>,
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
    /// Errors collected during the refresh. Surface as toasts; do not
    /// blank the existing data on a partial failure.
    pub errors: Vec<String>,
}

/// Spawn a worker that recomputes the full repo state off-thread.
/// `show_orphaned_commits` toggles the reflog-walk that brings back
/// commits unreachable from any current ref.
pub(crate) fn spawn_repo_state_refresh(
    repo_context_path: PathBuf,
    staging_context_path: Option<PathBuf>,
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
                    current_branch: String::new(),
                    head_oid: None,
                    worktrees: Vec::new(),
                    remote_names: Vec::new(),
                    remote_urls: HashMap::new(),
                    is_bare: false,
                    submodules: Vec::new(),
                    stashes: Vec::new(),
                    ahead_behind: HashMap::new(),
                    ref_fingerprint: 0,
                    real_oids: Vec::new(),
                    worktree_repos: HashMap::new(),
                    errors,
                });
                let _ = proxy.send_event(());
                return;
            }
        };

        let staging_repo = staging_context_path
            .as_ref()
            .and_then(|dir| GitRepo::open(dir).ok());
        let staging = staging_repo.as_ref().unwrap_or(&repo);

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

        let mut branch_tips = repo.branch_tips().unwrap_or_else(|e| {
            errors.push(format!("Failed to load branches: {e}"));
            Vec::new()
        });
        let tags = repo.tags().unwrap_or_else(|e| {
            errors.push(format!("Failed to load tags: {e}"));
            Vec::new()
        });
        let current_branch = staging.current_branch().unwrap_or_else(|e| {
            errors.push(format!("Failed to get current branch: {e}"));
            String::new()
        });
        let head_oid = staging.head_oid().ok();

        // Patch is_head against the staging context — for multi-worktree
        // repos this can differ from the main repo's HEAD.
        for tip in &mut branch_tips {
            tip.is_head = tip.name == current_branch && !tip.is_remote;
        }

        let worktrees = repo.worktrees().unwrap_or_else(|e| {
            errors.push(format!("Failed to load worktrees: {e}"));
            Vec::new()
        });

        // Open worktree repos on the worker — see struct doc above for
        // why this can't live on the main thread.
        let worktree_repos: HashMap<PathBuf, GitRepo> = worktrees
            .iter()
            .filter_map(|wt| {
                let path = PathBuf::from(&wt.path);
                GitRepo::open(&path).ok().map(|r| (path, r))
            })
            .collect();

        let remote_names = repo.remote_names();
        let is_bare = repo.is_effectively_bare();
        let remote_urls: HashMap<String, String> = remote_names
            .iter()
            .filter_map(|name| repo.remote_url(name).map(|url| (name.clone(), url)))
            .collect();

        let submodules = staging.submodules().unwrap_or_else(|e| {
            errors.push(format!("Failed to load submodules: {e}"));
            Vec::new()
        });

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
            current_branch,
            head_oid,
            worktrees,
            remote_names,
            remote_urls,
            is_bare,
            submodules,
            stashes,
            ahead_behind,
            ref_fingerprint,
            real_oids,
            worktree_repos,
            errors,
        });
        let _ = proxy.send_event(());
    });
    rx
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
        status: WorkingDirStatus,
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
            let status = check_worktree_status(&wt_path);
            let _ = tx.send(DirtyCheckResult::Worktree {
                tab_id,
                path: wt_path,
                status,
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
/// dirty check would recurse into nested sub-submodules.
fn check_dirty(path: &PathBuf) -> bool {
    let Ok(repo) = git2::Repository::open(path) else {
        return false;
    };
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true).exclude_submodules(true);
    repo.statuses(Some(&mut opts)).is_ok_and(|s| {
        s.iter()
            .any(|e| !e.status().intersects(git2::Status::IGNORED))
    })
}

/// Worktree variant — same exclude_submodules check, but returns the
/// full status so the tab's `WorktreeView` cache remains the single
/// source of truth for staging rows, WT pills, and synthetic commits.
fn check_worktree_status(path: &PathBuf) -> WorkingDirStatus {
    let Ok(repo) = git2::Repository::open(path) else {
        return WorkingDirStatus::default();
    };
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .exclude_submodules(true);
    repo.statuses(Some(&mut opts))
        .map(|statuses| working_dir_status_from_statuses(&statuses))
        .unwrap_or_default()
}
