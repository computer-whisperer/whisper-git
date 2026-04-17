use std::sync::mpsc::Receiver;

use git2::Oid;
use winit::event_loop::EventLoopProxy;

use crate::git::{RemoteOpResult, WorktreeInfo};
use crate::views::{BranchSidebar, CommitDetailView, CommitGraphView, DiffView, StagingWell};

use super::RightPanelMode;

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
