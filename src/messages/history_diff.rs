use crate::git::{CommitInfo, GitRepo};
use crate::ui::widgets::ToastManager;

use super::{AppMessage, MessageContext, MessageViewState};

mod commit_view;
mod graph_nav;

use commit_view::handle_commit_view_message;
use graph_nav::handle_graph_navigation_message;

/// Handle commit selection, diff viewing, and history navigation messages.
/// Returns `Some(handled)` when the message belongs to this domain.
pub(super) fn handle_history_diff_message(
    msg: &AppMessage,
    repo: &GitRepo,
    staging_repo: &GitRepo,
    commits: &mut Vec<CommitInfo>,
    view_state: &mut MessageViewState<'_>,
    toast_manager: &mut ToastManager,
    ctx: &MessageContext,
) -> Option<bool> {
    if let Some(handled) =
        handle_commit_view_message(msg, repo, staging_repo, commits, view_state, toast_manager)
    {
        return Some(handled);
    }
    handle_graph_navigation_message(msg, repo, commits, view_state, toast_manager, ctx)
}
