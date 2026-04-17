//! AppMessage enum for decoupled event handling.
//!
//! Defines the message protocol between UI interactions and git operations. Uses MessageViewState
//! borrow pattern to access only needed tab state fields. Message dispatch routes to git operations.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use git2::Oid;

use crate::git::{CommitInfo, GitRepo, RemoteOpResult};
use crate::ui::Rect;
use crate::ui::widgets::{ToastManager, ToastSeverity};

mod history_diff;
mod reload_diagnostics;
mod remote_sync;
mod repo_ops;
mod staging_commit;
mod view_state;

use history_diff::handle_history_diff_message;
pub use reload_diagnostics::{RepoStateSnapshot, compute_reload_deltas};
use remote_sync::handle_remote_sync_message;
use repo_ops::handle_repo_ops_message;
use staging_commit::handle_staging_commit_message;
pub use view_state::MessageViewState;

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
