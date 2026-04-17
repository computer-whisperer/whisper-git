use crate::git::GitRepo;
use crate::ui::widgets::ToastManager;

use super::{AppMessage, MessageViewState};

mod async_ops;
mod mutation_ops;

use async_ops::handle_repo_async_ops_message;
use mutation_ops::handle_repo_mutation_ops_message;

/// Handle repo mutation and async-op messages that are not staging/remote-sync/history.
/// Returns `Some(handled)` when the message belongs to this domain.
pub(super) fn handle_repo_ops_message(
    msg: &AppMessage,
    repo: &GitRepo,
    staging_repo: &GitRepo,
    view_state: &mut MessageViewState<'_>,
    toast_manager: &mut ToastManager,
) -> Option<bool> {
    if let Some(handled) =
        handle_repo_mutation_ops_message(msg, repo, staging_repo, view_state, toast_manager)
    {
        return Some(handled);
    }
    handle_repo_async_ops_message(msg, repo, staging_repo, view_state, toast_manager)
}
