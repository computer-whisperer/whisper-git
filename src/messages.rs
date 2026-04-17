//! AppMessage enum for decoupled event handling.
//!
//! Defines the message protocol between UI interactions and git operations. Uses MessageViewState
//! borrow pattern to access only needed tab state fields. Message dispatch routes to git operations.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use git2::Oid;
use winit::event_loop::EventLoopProxy;

use crate::git::{CommitInfo, GitRepo, RemoteOpResult, WorktreeInfo};
use crate::ui::Rect;
use crate::ui::widgets::{ToastManager, ToastSeverity};
use crate::views::{BranchSidebar, CommitDetailView, CommitGraphView, DiffView, StagingWell};

mod history_diff;
mod remote_sync;
mod repo_ops;
mod staging_commit;

use history_diff::handle_history_diff_message;
use remote_sync::handle_remote_sync_message;
use repo_ops::handle_repo_ops_message;
use staging_commit::handle_staging_commit_message;

/// What content mode the right panel is showing
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RightPanelMode {
    /// Default: file lists + commit message + buttons (upper), selected file diff (lower)
    #[default]
    Staging,
    /// Shown when a commit is selected in graph: commit detail (upper), file diff (lower)
    Browse,
}

/// Application-level messages for state changes
#[derive(Clone, Debug)]
pub enum AppMessage {
    StageFile(String),
    UnstageFile(String),
    StageAll,
    UnstageAll,
    Commit(String),
    Fetch(Option<String>),
    Pull {
        remote: Option<String>,
        branch: String,
    },
    PullRebase {
        remote: Option<String>,
        branch: String,
    },
    ShowPullDialog(String), // (branch) — caller specifies which branch
    PullBranchFrom {
        remote: String,
        branch: String,
        rebase: bool,
    },
    Push {
        remote: Option<String>,
        branch: String,
    },
    PushForce {
        remote: Option<String>,
        branch: String,
    },
    ShowPushDialog(String), // (branch) — caller specifies which branch
    PushBranchTo {
        local_branch: String,
        remote: String,
        remote_branch: String,
        force: bool,
    },
    SelectedCommit(Oid),
    ViewCommitFileDiff(Oid, String),
    ViewDiff(String, bool), // (path, staged)
    CheckoutBranch(String),
    CheckoutRemoteBranch(String, String),
    CheckoutCommit(Oid, Option<PathBuf>), // (commit_oid, target_worktree_dir)
    DeleteBranch(String),
    RenameBranch(String, String), // (old_name, new_name)
    StageHunk(String, usize),     // (file_path, hunk_index)
    UnstageHunk(String, usize),   // (file_path, hunk_index)
    DiscardFile(String),
    DiscardFiles(Vec<String>),
    DiscardHunk(String, usize), // (file_path, hunk_index)
    LoadMoreCommits,
    DeleteSubmodule(String),
    UpdateSubmodule(String),
    ResetSubmodule(String),
    JumpToWorktreeBranch(String),
    JumpToCommit(Oid),
    RemoveWorktree(String),
    MergeBranch(String, Option<PathBuf>), // (branch, target_worktree_dir)
    MergeNoFf(String, String, Option<PathBuf>), // (branch, commit_message, target_worktree_dir)
    MergeFfOnly(String, Option<PathBuf>),
    MergeSquash(String, Option<PathBuf>),
    RebaseBranchWithOptions(String, bool, bool, Option<PathBuf>), // (branch, autostash, rebase_merges, target_worktree_dir)
    CreateBranch(String, Oid),                                    // (name, at_commit)
    CreateTag(String, Oid),                                       // (name, at_commit)
    DeleteTag(String),
    StashPush,
    StashPop,
    StashApply(usize),
    StashDrop(usize),
    StashPopIndex(usize),
    CherryPick(Oid, Option<PathBuf>),
    AmendCommit(String),
    ToggleAmend,
    RevertCommit(Oid, Option<PathBuf>),
    ResetToCommit(Oid, git2::ResetType, Option<PathBuf>),
    EnterSubmodule(String),
    ExitSubmodule,
    ExitToDepth(usize),
    AbortOperation,
    CreateWorktree(String, String, bool, bool), // (name, source_ref, init_submodules, checkout_lfs)
    AddRemote(String, String),                  // (name, url)
    DeleteRemote(String),
    RenameRemote(String, String),       // (old_name, new_name)
    SetRemoteUrl(String, String),       // (name, new_url)
    DeleteRemoteBranch(String, String), // (remote, branch)
    FetchAll,
    CheckoutBranchInWorktree(String, PathBuf), // (branch, worktree_path)
    SetHead(String),                           // bare-repo HEAD pointer update
    StageAllUntracked,
    AiGenerateCommitMessage,
}

/// Try to set the generic async operation receiver. Returns `true` if the
/// operation was successfully queued, or `false` if another operation is
/// already in progress (in which case a toast is shown).
pub fn queue_async_op(
    generic_op_receiver: &mut Option<(Receiver<RemoteOpResult>, String, std::time::Instant)>,
    rx: Receiver<RemoteOpResult>,
    label: String,
    in_progress_msg: String,
    toast_manager: &mut ToastManager,
) -> bool {
    if generic_op_receiver.is_some() {
        toast_manager.push(
            "Another operation is in progress".to_string(),
            ToastSeverity::Info,
        );
        return false;
    }
    *generic_op_receiver = Some((rx, label, std::time::Instant::now()));
    toast_manager.push(in_progress_msg, ToastSeverity::Info);
    true
}

/// Context needed by [`handle_app_message`] that lives outside the per-tab
/// state. The caller constructs this from `App` fields before entering the
/// message loop.
pub struct MessageContext {
    /// Graph bounds for scroll-to-selection (JumpToWorktreeBranch).
    /// Compute this from the current layout before calling the handler.
    pub graph_bounds: Rect,
    /// Whether to include orphaned commits when refreshing the commit graph.
    pub show_orphaned_commits: bool,
}

/// Execute a synchronous git operation that mutates repo state, then request
/// an async refresh. On success, shows `success_msg` as a toast and sets
/// `needs_repo_refresh`. On error, shows `error_msg_prefix: <error>`.
pub(super) fn handle_repo_mutation(
    result: Result<(), anyhow::Error>,
    success_msg: String,
    error_msg_prefix: &str,
    view_state: &mut MessageViewState<'_>,
    toast_manager: &mut ToastManager,
) {
    match result {
        Ok(()) => {
            view_state.needs_repo_refresh = true;
            toast_manager.push(success_msg, ToastSeverity::Success);
        }
        Err(e) => {
            toast_manager.push(format!("{}: {}", error_msg_prefix, e), ToastSeverity::Error);
        }
    }
}

/// Resolve a target working directory for an operation. If an explicit target
/// worktree path is given, open a repo at that path and return its command dir.
/// Otherwise fall back to staging_repo's command dir. This decouples operations
/// from the "currently viewed worktree" UI state.
pub(super) fn resolve_cmd_dir(target_dir: &Option<PathBuf>, staging_repo: &GitRepo) -> PathBuf {
    if let Some(dir) = target_dir
        && let Ok(target_repo) = GitRepo::open(dir)
    {
        return target_repo.git_command_dir();
    }
    staging_repo.git_command_dir()
}

/// Dispatch a single `AppMessage`.
///
/// Returns `true` if the message was handled (even if it resulted in an
/// error toast). Returns `false` only when a prerequisite was not met and
/// the message was silently skipped (e.g. generic_op_receiver busy).
pub fn handle_app_message(
    msg: AppMessage,
    repo: &GitRepo,
    staging_repo: &GitRepo,
    commits: &mut Vec<CommitInfo>,
    view_state: &mut MessageViewState<'_>,
    toast_manager: &mut ToastManager,
    ctx: &MessageContext,
) -> bool {
    crate::crash_log::breadcrumb(format!("msg: {msg:?}"));
    let proxy = view_state.proxy.clone();
    if let Some(handled) =
        handle_staging_commit_message(&msg, staging_repo, view_state, toast_manager)
    {
        return handled;
    }
    if let Some(handled) = handle_remote_sync_message(&msg, repo, view_state, toast_manager, &proxy)
    {
        return handled;
    }
    if let Some(handled) = handle_history_diff_message(
        &msg,
        repo,
        staging_repo,
        commits,
        view_state,
        toast_manager,
        ctx,
    ) {
        return handled;
    }
    if let Some(handled) =
        handle_repo_ops_message(&msg, repo, staging_repo, view_state, toast_manager)
    {
        return handled;
    }
    match msg {
        // Handled in main.rs before message dispatch (needs App-level state)
        AppMessage::AiGenerateCommitMessage => {
            return false;
        }

        // Submodule navigation messages are handled in main.rs process_messages,
        // not here. If they leak through, just ignore them.
        AppMessage::EnterSubmodule(_) | AppMessage::ExitSubmodule | AppMessage::ExitToDepth(_) => {
            return false;
        }
        _ => {
            debug_assert!(false, "message should have been handled by domain handler");
            return true;
        }
    }
}

/// A borrowing view into `TabViewState` fields needed by the message handler.
///
/// This avoids passing the entire `TabViewState` (which contains fields
/// unrelated to message handling) and makes the required dependencies
/// explicit.
pub struct MessageViewState<'a> {
    pub commit_graph_view: &'a mut CommitGraphView,
    pub staging_well: &'a mut StagingWell,
    pub diff_view: &'a mut DiffView,
    pub commit_detail_view: &'a mut CommitDetailView,
    pub branch_sidebar: &'a mut BranchSidebar,
    pub header_bar: &'a mut crate::ui::widgets::HeaderBar,
    pub last_diff_commit: &'a mut Option<Oid>,
    pub fetch_receiver: &'a mut Option<(Receiver<RemoteOpResult>, std::time::Instant, String)>,
    pub pull_receiver: &'a mut Option<(Receiver<RemoteOpResult>, std::time::Instant, String)>,
    pub push_receiver: &'a mut Option<(Receiver<RemoteOpResult>, std::time::Instant, String)>,
    pub generic_op_receiver: &'a mut Option<(Receiver<RemoteOpResult>, String, std::time::Instant)>,
    pub right_panel_mode: &'a mut RightPanelMode,
    pub worktrees: &'a mut Vec<WorktreeInfo>,
    pub proxy: EventLoopProxy<()>,
    /// Set by message handlers to request an async repo state refresh
    /// (commit graph, branch tips, tags, etc.) after the message loop.
    pub needs_repo_refresh: bool,
}

/// Lightweight snapshot of diffable repo state for diagnostic reload comparison.
type SubmoduleSnapshot = (String, Option<bool>, Option<Oid>, Option<Oid>, Option<Oid>);

pub struct RepoStateSnapshot {
    pub commit_oids: Vec<Oid>,
    pub head_oid: Option<Oid>,
    pub current_branch: String,
    pub branch_tips: Vec<(String, Oid, bool)>,
    pub tags: Vec<(String, Oid)>,
    pub stashes: Vec<(usize, String)>,
    pub worktrees: Vec<(String, Option<bool>, Option<usize>)>,
    pub staged_count: usize,
    pub unstaged_count: usize,
    pub untracked_count: usize,
    pub conflicted_count: usize,
    pub ahead_behind: HashMap<String, (usize, usize)>,
    pub submodules: Vec<SubmoduleSnapshot>, // (path, is_dirty, head_pin_oid, index_pin_oid, workdir_oid)
}

impl RepoStateSnapshot {
    /// Capture what the UI currently believes from cached view state.
    pub fn from_ui(
        commits: &[CommitInfo],
        view_state: &MessageViewState<'_>,
        current_branch: &str,
        head_oid: Option<Oid>,
    ) -> Self {
        let commit_oids: Vec<Oid> = commits
            .iter()
            .filter(|c| !c.is_synthetic)
            .map(|c| c.id)
            .collect();

        let branch_tips: Vec<(String, Oid, bool)> = view_state
            .commit_graph_view
            .branch_tips
            .iter()
            .map(|t| (t.name.clone(), t.oid, t.is_remote))
            .collect();

        let tags: Vec<(String, Oid)> = view_state
            .commit_graph_view
            .tags
            .iter()
            .map(|t| (t.name.clone(), t.oid))
            .collect();

        let stashes: Vec<(usize, String)> = view_state
            .branch_sidebar
            .stashes
            .iter()
            .map(|s| (s.index, s.message.clone()))
            .collect();

        let worktrees: Vec<(String, Option<bool>, Option<usize>)> = view_state
            .worktrees
            .iter()
            .map(|w| (w.name.clone(), w.is_dirty, w.dirty_file_count))
            .collect();

        let staged_count = view_state.staging_well.staged_list.files.len();
        let unstaged_count = view_state.staging_well.unstaged_list.files.len();
        let untracked_count = view_state.staging_well.untracked_list.files.len();
        let conflicted_count = view_state.staging_well.conflicted_list.files.len();

        let ahead_behind = view_state.branch_sidebar.ahead_behind_cache();

        let submodules: Vec<SubmoduleSnapshot> = view_state
            .staging_well
            .submodules
            .iter()
            .map(|s| {
                (
                    s.path.clone(),
                    s.is_dirty,
                    s.head_oid,
                    s.index_oid,
                    s.workdir_oid,
                )
            })
            .collect();

        let current_branch = current_branch.to_string();

        Self {
            commit_oids,
            head_oid,
            current_branch,
            branch_tips,
            tags,
            stashes,
            worktrees,
            staged_count,
            unstaged_count,
            untracked_count,
            conflicted_count,
            ahead_behind,
            submodules,
        }
    }
}

/// Compare two snapshots and produce human-readable delta descriptions.
pub fn compute_reload_deltas(before: &RepoStateSnapshot, after: &RepoStateSnapshot) -> Vec<String> {
    let mut deltas = Vec::new();

    // Commits: set diff
    {
        use std::collections::HashSet;
        let before_set: HashSet<&Oid> = before.commit_oids.iter().collect();
        let after_set: HashSet<&Oid> = after.commit_oids.iter().collect();
        let added = after_set.difference(&before_set).count();
        let removed = before_set.difference(&after_set).count();
        if added > 0 || removed > 0 {
            deltas.push(format!("Commits: +{} added, -{} removed", added, removed));
        }
    }

    // HEAD
    if before.head_oid != after.head_oid {
        let fmt_oid = |o: &Option<Oid>| match o {
            Some(oid) => oid.to_string()[..7].to_string(),
            None => "None".to_string(),
        };
        deltas.push(format!(
            "HEAD moved: {} -> {}",
            fmt_oid(&before.head_oid),
            fmt_oid(&after.head_oid)
        ));
    }

    // Current branch
    if before.current_branch != after.current_branch {
        deltas.push(format!(
            "Branch: '{}' -> '{}'",
            before.current_branch, after.current_branch
        ));
    }

    // Branch tips
    {
        let before_map: HashMap<(&str, bool), Oid> = before
            .branch_tips
            .iter()
            .map(|(n, o, r)| ((n.as_str(), *r), *o))
            .collect();
        let after_map: HashMap<(&str, bool), Oid> = after
            .branch_tips
            .iter()
            .map(|(n, o, r)| ((n.as_str(), *r), *o))
            .collect();
        for (key, oid) in &after_map {
            match before_map.get(key) {
                None => deltas.push(format!(
                    "Branch added: {}{}",
                    if key.1 { "(remote) " } else { "" },
                    key.0
                )),
                Some(old_oid) if old_oid != oid => {
                    deltas.push(format!(
                        "Branch moved: {} {} -> {}",
                        key.0,
                        &old_oid.to_string()[..7],
                        &oid.to_string()[..7]
                    ));
                }
                _ => {}
            }
        }
        for key in before_map.keys() {
            if !after_map.contains_key(key) {
                deltas.push(format!(
                    "Branch removed: {}{}",
                    if key.1 { "(remote) " } else { "" },
                    key.0
                ));
            }
        }
    }

    // Tags
    {
        let before_tags: HashMap<&str, Oid> =
            before.tags.iter().map(|(n, o)| (n.as_str(), *o)).collect();
        let after_tags: HashMap<&str, Oid> =
            after.tags.iter().map(|(n, o)| (n.as_str(), *o)).collect();
        for name in after_tags.keys() {
            if !before_tags.contains_key(name) {
                deltas.push(format!("Tag added: {}", name));
            }
        }
        for name in before_tags.keys() {
            if !after_tags.contains_key(name) {
                deltas.push(format!("Tag removed: {}", name));
            }
        }
    }

    // Stashes
    if before.stashes.len() != after.stashes.len() {
        deltas.push(format!(
            "Stashes: {} -> {}",
            before.stashes.len(),
            after.stashes.len()
        ));
    }

    // Worktrees
    for after_wt in &after.worktrees {
        if let Some(before_wt) = before.worktrees.iter().find(|w| w.0 == after_wt.0) {
            if before_wt.1 != after_wt.1 || before_wt.2 != after_wt.2 {
                deltas.push(format!(
                    "Worktree '{}': dirty {:?}({:?}) -> {:?}({:?})",
                    after_wt.0, before_wt.1, before_wt.2, after_wt.1, after_wt.2
                ));
            }
        } else {
            deltas.push(format!("Worktree added: {}", after_wt.0));
        }
    }
    for before_wt in &before.worktrees {
        if !after.worktrees.iter().any(|w| w.0 == before_wt.0) {
            deltas.push(format!("Worktree removed: {}", before_wt.0));
        }
    }

    // Status counts
    if before.staged_count != after.staged_count {
        deltas.push(format!(
            "Staged: {} -> {}",
            before.staged_count, after.staged_count
        ));
    }
    if before.unstaged_count != after.unstaged_count {
        deltas.push(format!(
            "Unstaged: {} -> {}",
            before.unstaged_count, after.unstaged_count
        ));
    }
    if before.untracked_count != after.untracked_count {
        deltas.push(format!(
            "Untracked: {} -> {}",
            before.untracked_count, after.untracked_count
        ));
    }
    if before.conflicted_count != after.conflicted_count {
        deltas.push(format!(
            "Conflicted: {} -> {}",
            before.conflicted_count, after.conflicted_count
        ));
    }

    // Ahead/behind
    {
        let mut all_branches: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for key in before.ahead_behind.keys() {
            all_branches.insert(key.as_str());
        }
        for key in after.ahead_behind.keys() {
            all_branches.insert(key.as_str());
        }
        for branch in all_branches {
            let b = before.ahead_behind.get(branch).copied().unwrap_or((0, 0));
            let a = after.ahead_behind.get(branch).copied().unwrap_or((0, 0));
            if b != a {
                deltas.push(format!(
                    "Ahead/behind '{}': ({},{}) -> ({},{})",
                    branch, b.0, b.1, a.0, a.1
                ));
            }
        }
    }

    // Submodules
    for after_sm in &after.submodules {
        if let Some(before_sm) = before.submodules.iter().find(|s| s.0 == after_sm.0) {
            if before_sm.1 != after_sm.1 {
                deltas.push(format!(
                    "Submodule '{}': dirty {:?} -> {:?}",
                    after_sm.0, before_sm.1, after_sm.1
                ));
            }
            let fmt = |oid: Option<Oid>| {
                oid.map(|o| o.to_string()[..7].to_string())
                    .unwrap_or_else(|| "None".to_string())
            };
            if before_sm.2 != after_sm.2 {
                deltas.push(format!(
                    "Submodule '{}': HEAD pin {} -> {}",
                    after_sm.0,
                    fmt(before_sm.2),
                    fmt(after_sm.2)
                ));
            }
            if before_sm.3 != after_sm.3 {
                deltas.push(format!(
                    "Submodule '{}': index pin {} -> {}",
                    after_sm.0,
                    fmt(before_sm.3),
                    fmt(after_sm.3)
                ));
            }
            if before_sm.4 != after_sm.4 {
                deltas.push(format!(
                    "Submodule '{}': workdir {} -> {}",
                    after_sm.0,
                    fmt(before_sm.4),
                    fmt(after_sm.4)
                ));
            }
        } else {
            deltas.push(format!("Submodule added: {}", after_sm.0));
        }
    }
    for before_sm in &before.submodules {
        if !after.submodules.iter().any(|s| s.0 == before_sm.0) {
            deltas.push(format!("Submodule removed: {}", before_sm.0));
        }
    }

    deltas
}
