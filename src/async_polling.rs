//! Async operation polling, background status/repo-state refresh, and CI fetch triggers.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use git2::Oid;
use winit::event_loop::EventLoopProxy;

use crate::config;
use crate::git::{
    self, BranchTip, CommitInfo, GitRepo, RemoteOpResult, StashEntry, SubmoduleInfo, TagInfo,
    WorkingDirStatus, WorktreeInfo,
};
use crate::github;
use crate::gitlab;
use crate::token_store;
use crate::ui::widgets::{ToastManager, ToastSeverity};

use super::{MAX_COMMITS, RepoTab, TabViewState};

/// Result of polling an async remote operation receiver.
pub(crate) enum AsyncOpPoll {
    /// Operation completed successfully; contains the remote/op name for the toast.
    Success(String),
    /// Operation failed; contains (friendly_message, raw_stderr).
    Failed(String, String),
    /// Background thread disconnected unexpectedly.
    Disconnected,
    /// Timeout threshold reached — caller should show a "still running" toast.
    Timeout,
    /// Still running, nothing to report yet.
    Pending,
}

/// Trigger async CI status fetches for all detected providers (GitHub, GitLab).
pub(crate) fn trigger_ci_fetch(
    config: &config::Config,
    repo_tab: &RepoTab,
    view_state: &mut TabViewState,
    proxy: &EventLoopProxy<()>,
) {
    // Collect all remote URLs (prefer "origin" first)
    let mut remote_urls: Vec<String> = Vec::new();
    if let Some(url) = repo_tab.repo.remote_url("origin") {
        remote_urls.push(url);
    }
    for name in repo_tab.repo.remote_names() {
        if name != "origin"
            && let Some(url) = repo_tab.repo.remote_url(&name)
        {
            remote_urls.push(url);
        }
    }

    let mut receivers = Vec::new();

    // GitHub: try to find a GitHub remote and fetch CI
    // Check keychain first, fall back to plaintext config
    let github_token = token_store::get_github_token()
        .or_else(|| config.github_token.clone())
        .filter(|t| !t.is_empty());
    if let Some(token) = github_token
        && let Some(url) = remote_urls
            .iter()
            .find(|u| github::parse_github_remote(u).is_some())
        && let Some(rx) = github::fetch_ci_status_async(&token, url, proxy.clone())
    {
        receivers.push(rx);
    }

    // GitLab: try to find a GitLab remote with a matching token
    for url in &remote_urls {
        if let Some(remote) = gitlab::parse_gitlab_remote(url) {
            // Check keychain first, fall back to plaintext config
            let token = token_store::get_gitlab_token(
                remote
                    .api_base
                    .strip_prefix("https://")
                    .unwrap_or(&remote.api_base),
            )
            .or_else(|| {
                config
                    .gitlab_token_for_host(&remote.api_base)
                    .map(|s| s.to_string())
            })
            .filter(|t| !t.is_empty());
            if let Some(token) = token {
                if let Some(rx) = gitlab::fetch_ci_status_async(&token, url, proxy.clone()) {
                    receivers.push(rx);
                }
                break;
            }
        }
    }

    if !receivers.is_empty() {
        view_state.ci_receivers = receivers;
        view_state.last_ci_fetch = Instant::now();
    }
}

/// Poll a remote operation receiver (fetch/pull/push) and return what happened.
/// On completion or disconnect, clears the receiver, header flag, and timeout flag.
/// On timeout, sets the timeout flag.
pub(crate) fn poll_remote_op(
    receiver: &mut Option<(Receiver<RemoteOpResult>, Instant, String)>,
    header_flag: &mut bool,
    timeout_flag: &mut bool,
    op_name: &str,
    now: Instant,
    timeout_secs: u64,
) -> AsyncOpPoll {
    use std::sync::mpsc::TryRecvError;

    let Some((ref rx, started, ref remote_name)) = *receiver else {
        return AsyncOpPoll::Pending;
    };
    match rx.try_recv() {
        Ok(result) => {
            let remote = remote_name.clone();
            *header_flag = false;
            *receiver = None;
            *timeout_flag = false;
            if result.success {
                AsyncOpPoll::Success(remote)
            } else {
                let (msg, _) = git::classify_git_error(op_name, &result.error);
                AsyncOpPoll::Failed(msg, result.error)
            }
        }
        Err(TryRecvError::Disconnected) => {
            *header_flag = false;
            *receiver = None;
            *timeout_flag = false;
            AsyncOpPoll::Disconnected
        }
        Err(TryRecvError::Empty) => {
            if now.duration_since(started).as_secs() >= timeout_secs && !*timeout_flag {
                *timeout_flag = true;
                AsyncOpPoll::Timeout
            } else {
                AsyncOpPoll::Pending
            }
        }
    }
}

// ============================================================================
// Async status refresh
// ============================================================================

/// Result of a background status refresh.
pub(crate) struct StatusResult {
    /// Main repo working directory status (for graph + header)
    pub main_status: Option<WorkingDirStatus>,
    /// Staging repo (worktree) status (for staging well)
    pub staging_status: Option<WorkingDirStatus>,
    /// Staging repo state (merge/rebase in progress, etc.)
    pub staging_repo_state: git2::RepositoryState,
    /// Submodules for the active staging context.
    pub submodules: Vec<SubmoduleInfo>,
    /// Per-worktree dirty flags: (path, is_dirty, dirty_file_count)
    pub worktree_dirty: Vec<(String, bool, usize)>,
    /// Pre-computed diff stats for the main repo working tree (insertions, deletions).
    pub main_diff_stats: Option<(usize, usize)>,
    /// Pre-computed diff stats for each dirty worktree: (path, insertions, deletions)
    pub worktree_diff_stats: Vec<(String, usize, usize)>,
    /// HEAD OID of the main repo (for synthetic entry parent linkage)
    pub head_oid: Option<Oid>,
    /// Workdir path of the main repo
    pub workdir: Option<String>,
}

/// Spawn a background thread to compute working directory status.
pub(crate) fn spawn_status_refresh(
    repo_context_path: PathBuf,
    staging_context_path: Option<PathBuf>,
    worktree_paths: Vec<String>,
    is_bare: bool,
    proxy: EventLoopProxy<()>,
) -> Receiver<StatusResult> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let main_repo = git2::Repository::open(&repo_context_path).ok();

        let main_status = if !is_bare {
            main_repo.as_ref().and_then(|repo| {
                let mut opts = git2::StatusOptions::new();
                opts.include_untracked(true).recurse_untracked_dirs(true);
                let statuses = repo.statuses(Some(&mut opts)).ok()?;
                Some(crate::git::working_dir_status_from_statuses(&statuses))
            })
        } else {
            Some(WorkingDirStatus::default())
        };

        let (staging_status, staging_repo_state) = match staging_context_path
            .as_ref()
            .and_then(|dir| git2::Repository::open(dir).ok())
        {
            Some(repo) => {
                let state = repo.state();
                let status = if !is_bare {
                    let mut opts = git2::StatusOptions::new();
                    opts.include_untracked(true).recurse_untracked_dirs(true);
                    repo.statuses(Some(&mut opts))
                        .ok()
                        .map(|s| crate::git::working_dir_status_from_statuses(&s))
                } else {
                    Some(WorkingDirStatus::default())
                };
                (status, state)
            }
            None => (None, git2::RepositoryState::Clean),
        };

        let submodules = staging_context_path
            .as_ref()
            .and_then(|dir| GitRepo::open(dir).ok())
            .and_then(|repo| repo.submodules().ok())
            .unwrap_or_default();

        let worktree_dirty: Vec<(String, bool, usize)> = worktree_paths
            .into_iter()
            .filter_map(|path| {
                let repo = git2::Repository::open(&path).ok()?;
                let (dirty, count) = repo
                    .statuses(None)
                    .map(|statuses| {
                        let c = statuses
                            .iter()
                            .filter(|e| !e.status().intersects(git2::Status::IGNORED))
                            .count();
                        (c > 0, c)
                    })
                    .unwrap_or((false, 0));
                Some((path, dirty, count))
            })
            .collect();

        // Pre-compute diff stats for synthetic entries (expensive git operations
        // that must not run on the main thread to avoid Wayland disconnects).
        let (head_oid, workdir, main_diff_stats) = match main_repo.as_ref() {
            Some(repo) => {
                let head = repo.head().ok().and_then(|r| r.target());
                let wd = repo.workdir().map(|p| p.to_string_lossy().to_string());
                let has_dirty_files = main_status.as_ref().is_some_and(|s| s.total_files() > 0);
                let stats = if has_dirty_files {
                    Some(crate::git::GitRepo::diff_stats_raw(repo))
                } else {
                    None
                };
                (head, wd, stats)
            }
            None => (None, None, None),
        };

        let worktree_diff_stats: Vec<(String, usize, usize)> = worktree_dirty
            .iter()
            .filter(|(_, dirty, _)| *dirty)
            .filter_map(|(path, _, _)| {
                let repo = git2::Repository::open(path).ok()?;
                let (ins, del) = crate::git::GitRepo::diff_stats_raw(&repo);
                Some((path.clone(), ins, del))
            })
            .collect();

        let _ = tx.send(StatusResult {
            main_status,
            staging_status,
            staging_repo_state,
            submodules,
            worktree_dirty,
            main_diff_stats,
            worktree_diff_stats,
            head_oid,
            workdir,
        });
        let _ = proxy.send_event(());
    });
    rx
}

/// Apply a completed status result to the UI state.
pub(crate) fn apply_status_result(
    result: StatusResult,
    repo_tab: &mut RepoTab,
    view_state: &mut TabViewState,
) {
    if let Some(status) = result.staging_status {
        view_state.staging_well.update_status(&status);
    }
    view_state.staging_well.repo_state_label =
        crate::git::repo_state_label(result.staging_repo_state);
    view_state.staging_well.set_submodules(result.submodules);

    let main_dirty_count = result
        .main_status
        .as_ref()
        .map(|s| s.total_files())
        .unwrap_or(0);
    if let Some(status) = result.main_status {
        view_state.commit_graph_view.working_dir_status = Some(status);
    }

    let mut dirty_changed = false;
    for (path, dirty, count) in &result.worktree_dirty {
        if let Some(wt) = view_state
            .worktree_state
            .worktrees
            .iter_mut()
            .find(|w| &w.path == path)
        {
            if wt.is_dirty != *dirty || wt.dirty_file_count != *count {
                dirty_changed = true;
            }
            wt.is_dirty = *dirty;
            wt.dirty_file_count = *count;
        }
    }

    if dirty_changed {
        view_state
            .staging_well
            .set_worktrees(&view_state.worktree_state.worktrees);

        // Build synthetic entries from pre-computed background data (no git calls here).
        repo_tab.commits.retain(|c| !c.is_synthetic);
        let mut synthetics = Vec::new();

        if view_state.worktree_state.worktrees.is_empty() {
            // Single-worktree: use main repo diff stats
            if let (Some(head), Some(wd)) = (result.head_oid, &result.workdir)
                && main_dirty_count > 0
            {
                let count = main_dirty_count;
                let parent_time = repo_tab
                    .commits
                    .iter()
                    .find(|c| c.id == head)
                    .map(|c| c.time)
                    .unwrap_or(0);
                let mut entry = CommitInfo::synthetic_for_working_dir(head, count, wd, parent_time);
                if let Some((ins, del)) = result.main_diff_stats {
                    entry.insertions = ins;
                    entry.deletions = del;
                }
                synthetics.push(entry);
            }
        } else {
            // Multi-worktree: use per-worktree diff stats
            for wt in &view_state.worktree_state.worktrees {
                if wt.is_dirty {
                    let parent_time = wt
                        .head_oid
                        .and_then(|oid| repo_tab.commits.iter().find(|c| c.id == oid))
                        .map(|c| c.time)
                        .unwrap_or(0);
                    if let Some(mut synthetic) = CommitInfo::synthetic_for_worktree(wt, parent_time)
                    {
                        if let Some((_, ins, del)) = result
                            .worktree_diff_stats
                            .iter()
                            .find(|(p, _, _)| *p == wt.path)
                        {
                            synthetic.insertions = *ins;
                            synthetic.deletions = *del;
                        }
                        synthetics.push(synthetic);
                    }
                }
            }
        }

        if !synthetics.is_empty() {
            git::insert_synthetics_sorted(&mut repo_tab.commits, synthetics);
        }
        view_state
            .commit_graph_view
            .update_layout(&repo_tab.commits);
    }
}

// ============================================================================
// Async repo state refresh
// ============================================================================

/// Result of a background repo state refresh.
pub(crate) struct RepoStateResult {
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
    pub ref_fingerprint: u64,
    /// OIDs for which diff stats should be computed
    pub real_oids: Vec<Oid>,
    /// Pre-opened worktree repo handles (opened on background thread)
    pub worktree_repos: HashMap<PathBuf, GitRepo>,
    pub errors: Vec<String>,
}

/// Spawn a background thread to compute the full repo state refresh.
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

        // Commits
        let graph_result = if show_orphaned_commits {
            repo.commit_graph_with_orphans(MAX_COMMITS)
        } else {
            repo.commit_graph(MAX_COMMITS)
        };
        let mut commits = match graph_result {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("Failed to load commits: {e}"));
                Vec::new()
            }
        };

        // Branches
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

        // Fix is_head based on staging context
        for tip in &mut branch_tips {
            tip.is_head = tip.name == current_branch && !tip.is_remote;
        }

        // Worktrees
        let worktrees = repo.worktrees().unwrap_or_else(|e| {
            errors.push(format!("Failed to load worktrees: {e}"));
            Vec::new()
        });

        // Open worktree repos on the background thread to avoid blocking the
        // main thread (which causes Wayland disconnects on compositor timeout).
        let worktree_repos: HashMap<PathBuf, GitRepo> = worktrees
            .iter()
            .filter_map(|wt| {
                let path = PathBuf::from(&wt.path);
                GitRepo::open(&path).ok().map(|r| (path, r))
            })
            .collect();

        // Synthetic entries
        let synthetics = git::create_synthetic_entries(&repo, &worktrees, &commits);
        if !synthetics.is_empty() {
            git::insert_synthetics_sorted(&mut commits, synthetics);
        }

        // Remotes
        let remote_names = repo.remote_names();
        let is_bare = repo.is_effectively_bare();
        let remote_urls: HashMap<String, String> = remote_names
            .iter()
            .filter_map(|name| repo.remote_url(name).map(|url| (name.clone(), url)))
            .collect();

        // Submodules for the active staging context (selected worktree or current repo)
        let submodules = staging.submodules().unwrap_or_else(|e| {
            errors.push(format!("Failed to load submodules: {e}"));
            Vec::new()
        });

        // Stashes
        let stashes = repo.stash_list();

        // Ahead/behind for all branches
        let ahead_behind = repo.all_branches_ahead_behind();

        // Ref fingerprint
        let ref_fingerprint = git::ref_fingerprint(repo.git_dir());

        // Collect real OIDs for diff stats
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

/// Apply a completed repo state result to the UI.
/// Returns a diff stats receiver if OIDs are available.
pub(crate) fn apply_repo_state_result(
    result: RepoStateResult,
    repo_tab: &mut RepoTab,
    view_state: &mut TabViewState,
    toast_manager: &mut ToastManager,
    proxy: &EventLoopProxy<()>,
) -> Option<Receiver<Vec<(Oid, usize, usize)>>> {
    let diag = std::env::var_os("WHISPER_FRAME_DIAG").is_some();
    let t0 = std::time::Instant::now();

    // Report errors as toasts
    for err in &result.errors {
        toast_manager.push(err.clone(), ToastSeverity::Error);
    }

    // Preserve existing diff stats so they don't flicker away during refresh
    let prev_stats: HashMap<Oid, (usize, usize)> = repo_tab
        .commits
        .iter()
        .filter(|c| c.insertions > 0 || c.deletions > 0)
        .map(|c| (c.id, (c.insertions, c.deletions)))
        .collect();

    repo_tab.commits = result.commits;

    // Restore cached diff stats
    for commit in repo_tab.commits.iter_mut() {
        if let Some(&(ins, del)) = prev_stats.get(&commit.id) {
            commit.insertions = ins;
            commit.deletions = del;
        }
    }

    // Update views
    let t = std::time::Instant::now();
    view_state
        .commit_graph_view
        .update_layout(&repo_tab.commits);
    if diag {
        eprintln!("[frame_diag]   update_layout: {:.1}ms", t.elapsed().as_secs_f64() * 1000.0);
    }
    view_state.commit_graph_view.branch_tips = result.branch_tips;
    view_state.commit_graph_view.tags = result.tags.clone();

    let t = std::time::Instant::now();
    view_state.branch_sidebar.set_branch_data(
        &view_state.commit_graph_view.branch_tips,
        &result.tags,
        &result.remote_names,
        &result.remote_urls,
        &result.worktrees,
        result.is_bare,
    );
    if diag {
        eprintln!("[frame_diag]   set_branch_data: {:.1}ms", t.elapsed().as_secs_f64() * 1000.0);
    }
    view_state.staging_well.set_worktrees(&result.worktrees);

    // Update worktree state
    view_state.worktree_state.worktrees = result.worktrees;
    // Merge pre-opened worktree repos from background thread into the cache.
    for (path, repo) in result.worktree_repos {
        view_state
            .worktree_state
            .repo_cache
            .entry(path)
            .or_insert(repo);
    }
    // Prune stale cache entries
    let valid: HashSet<PathBuf> = view_state
        .worktree_state
        .worktrees
        .iter()
        .map(|wt| PathBuf::from(&wt.path))
        .collect();
    view_state
        .worktree_state
        .repo_cache
        .retain(|p, _| valid.contains(p));

    // Keep staging context aligned with the staging well's active worktree pill.
    // This makes submodule state worktree-scoped instead of repo-global.
    match view_state.staging_well.active_worktree_path() {
        Some(path) => view_state.worktree_state.select(path),
        None => view_state.worktree_state.selected_path = None,
    }

    let fallback_submodules = result.submodules;
    let fallback_branch = result.current_branch;
    let fallback_head = result.head_oid;
    let t = std::time::Instant::now();
    let (submodules, current_branch, head_oid, staging_repo_state) = {
        let staging_repo = view_state.worktree_state.staging_repo_or(&repo_tab.repo);
        let submodules = staging_repo.submodules().unwrap_or_else(|e| {
            toast_manager.push(
                format!("Failed to load submodules: {}", e),
                ToastSeverity::Error,
            );
            fallback_submodules
        });
        let current_branch = staging_repo.current_branch().unwrap_or(fallback_branch);
        let head_oid = staging_repo.head_oid().ok().or(fallback_head);
        (
            submodules,
            current_branch,
            head_oid,
            staging_repo.repo_state(),
        )
    };
    if diag {
        eprintln!("[frame_diag]   staging_repo queries: {:.1}ms", t.elapsed().as_secs_f64() * 1000.0);
    }
    view_state.staging_well.set_submodules(submodules);

    view_state.branch_sidebar.stashes = result.stashes;
    view_state
        .branch_sidebar
        .update_ahead_behind(result.ahead_behind);

    // Header
    let project_path = repo_tab
        .repo
        .common_dir()
        .parent()
        .unwrap_or(repo_tab.repo.common_dir());
    let repo_path_str = project_path.to_string_lossy().into_owned();
    let repo_path_str = repo_path_str.trim_end_matches('/').to_string();
    view_state.header_bar.set_repo_path(&repo_path_str);

    // Operation state
    view_state.header_bar.operation_state_label = git::repo_state_label(staging_repo_state);

    // Derive HEAD from worktree state
    view_state.current_branch = current_branch;
    view_state.head_oid = head_oid;
    for tip in &mut view_state.commit_graph_view.branch_tips {
        tip.is_head = !tip.is_remote && tip.name == view_state.current_branch;
    }

    // Update ref fingerprint
    view_state.ref_fingerprint = result.ref_fingerprint;

    if diag {
        eprintln!("[frame_diag]   apply_repo_state_result total: {:.1}ms", t0.elapsed().as_secs_f64() * 1000.0);
    }

    // Spawn async diff stats
    if !result.real_oids.is_empty() {
        Some(
            repo_tab
                .repo
                .compute_diff_stats_async(result.real_oids, proxy.clone()),
        )
    } else {
        None
    }
}
