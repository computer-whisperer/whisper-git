//! AppMessage enum for decoupled event handling.
//!
//! Defines the message protocol between UI interactions and git operations. Uses MessageViewState
//! borrow pattern to access only needed tab state fields. Message dispatch routes to git operations.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use crate::git::{CommitInfo, GitRepo, RemoteOpResult};
use crate::ui::widgets::{ToastManager, ToastSeverity};

mod app_message;
mod history_diff;
mod reload_diagnostics;
mod remote_sync;
mod repo_ops;
mod staging_commit;
mod types;
mod view_state;

pub use app_message::{AppMessage, MessageContext, RightPanelMode};
use history_diff::handle_history_diff_message;
pub use reload_diagnostics::{RepoStateSnapshot, compute_reload_deltas};
use remote_sync::handle_remote_sync_message;
use repo_ops::handle_repo_ops_message;
use staging_commit::handle_staging_commit_message;
pub use types::{GenericRemoteOpSlot, TimedRemoteOpSlot};
pub use view_state::MessageViewState;

/// Try to set the generic async operation receiver. Returns `true` if the
/// operation was successfully queued, or `false` if another operation is
/// already in progress (in which case a toast is shown).
pub fn queue_async_op(
    generic_op_receiver: &mut GenericRemoteOpSlot,
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
        AppMessage::AiGenerateCommitMessage => false,

        // Submodule navigation messages are handled in main.rs process_messages,
        // not here. If they leak through, just ignore them.
        AppMessage::EnterSubmodule(_) | AppMessage::ExitSubmodule | AppMessage::ExitToDepth(_) => {
            false
        }
        _ => {
            debug_assert!(false, "message should have been handled by domain handler");
            true
        }
    }
}
