//! Main application entry point and event loop.
//!
//! Owns the App struct, winit event loop, Vulkan draw pipeline, and three-layer rendering architecture
//! (base → chrome → overlay). Handles async git operations via mpsc channels and thread spawning.

mod ai;
mod app_async_polling;
mod app_bootstrap;
mod app_diagnostic_reload;
mod app_input_handlers;
mod app_message_processing;
mod app_modal_state;
mod app_repo_lifecycle;
mod app_scheduling;
mod app_settings_tokens;
mod app_shortcuts;
mod app_sidebar_actions;
mod app_tab_management;
mod app_window_events;
mod async_polling;
mod ci;
mod config;
mod crash_log;
mod git;
mod github;
mod gitlab;
mod input;
mod messages;
mod receiver_poll;
mod renderer;
mod rendering;
mod submodule_nav;
mod token_store;
mod ui;
mod views;
mod watcher;

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;
use std::time::Instant;
use vulkano::{
    Validated,
    command_buffer::{
        AutoCommandBufferBuilder, CommandBufferUsage, allocator::StandardCommandBufferAllocator,
    },
    image::ImageLayout,
    memory::allocator::StandardMemoryAllocator,
    sync::{self, GpuFuture},
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{CursorIcon, Window, WindowId},
};

use git2::Oid;

use crate::config::Config;
use crate::git::{CommitInfo, GitRepo, SubmoduleInfo, WorktreeInfo};
use crate::input::{InputEvent, InputState, Key};
use crate::messages::{
    AppMessage, GenericRemoteOpSlot, MessageContext, MessageViewState, RepoStateSnapshot,
    RightPanelMode, TimedRemoteOpSlot, compute_reload_deltas, handle_app_message,
};
use crate::receiver_poll::{ReceiverPoll, poll_slot};
use crate::renderer::{SurfaceManager, VulkanContext};
use crate::ui::widget::theme;
use crate::ui::widgets::{
    BranchNameDialog, BranchNameDialogAction, CloneDialog, CloneDialogAction, ConfirmDialog,
    ConfirmDialogAction, ContextMenu, ErrorDialog, HeaderBar, MenuAction, MergeDialog,
    MergeDialogAction, MergeStrategy, PullDialog, PullDialogAction, PushDialog, PushDialogAction,
    RebaseDialog, RebaseDialogAction, RemoteDialog, RemoteDialogAction, RepoDialog,
    RepoDialogAction, SettingsDialog, SettingsDialogAction, ShortcutBar, TabAction, TabBar,
    ToastManager, ToastSeverity, TokenDialog, TokenDialogAction, Tooltip,
};
use crate::ui::{
    AvatarCache, AvatarRenderer, IconRenderer, Rect, ScreenLayout, SplineRenderer, TextRenderer,
    Widget,
};
use crate::views::{
    BranchSidebar, CommitDetailAction, CommitDetailView, CommitGraphView, DiffAction, DiffView,
    GraphAction, SidebarAction, StagingAction, StagingWell,
};
use crate::watcher::{FsChangeKind, RepoWatcher};

use crate::async_polling::{
    AsyncOpPoll, RepoStateResult, StatusResult, apply_dirty_check_result, apply_repo_state_result,
    apply_status_result, poll_remote_op, spawn_dirty_checks, spawn_repo_state_refresh,
    spawn_status_refresh, trigger_ci_fetch,
};
use crate::rendering::{
    apply_screenshot_state, capture_screenshot, capture_screenshot_offscreen, draw_frame,
    handle_context_menu_action,
};
use crate::submodule_nav::{
    enter_submodule, exit_submodule, exit_to_depth, init_tab_view, open_terminal_at,
};

/// Maximum number of commits to load into the graph view.
pub(crate) const MAX_COMMITS: usize = 50;
pub(crate) type WatcherInitResult = Result<(RepoWatcher, Receiver<FsChangeKind>), String>;
/// Rebuild text atlases when display scale drifts too far from atlas build scale.
/// Hysteresis avoids rebuild churn from tiny fractional-scale fluctuations.
const TEXT_REBUILD_SCALE_UP_RATIO: f64 = 1.10;
const TEXT_REBUILD_SCALE_DOWN_RATIO: f64 = 0.80;

// ============================================================================
// CLI
// ============================================================================

#[derive(Default)]
struct CliArgs {
    screenshot: Option<PathBuf>,
    screenshot_size: Option<(u32, u32)>,
    screenshot_scale: Option<f64>,
    screenshot_state: Option<String>,
    view: Option<String>,
    repos: Vec<PathBuf>,
}

fn parse_args() -> CliArgs {
    let mut args = CliArgs::default();
    let mut iter = std::env::args().skip(1);

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--screenshot" => args.screenshot = iter.next().map(PathBuf::from),
            "--size" => {
                // Parse WxH format (e.g., "1920x1080")
                if let Some(size_str) = iter.next()
                    && let Some((w, h)) = size_str.split_once('x')
                    && let (Ok(width), Ok(height)) = (w.parse(), h.parse())
                {
                    args.screenshot_size = Some((width, height));
                }
            }
            "--scale" => {
                if let Some(s) = iter.next() {
                    args.screenshot_scale = s.parse().ok();
                }
            }
            "--screenshot-state" => args.screenshot_state = iter.next(),
            "--view" => args.view = iter.next(),
            "--repo" => {
                if let Some(p) = iter.next() {
                    args.repos.push(PathBuf::from(p));
                }
            }
            other if !other.starts_with('-') => args.repos.push(PathBuf::from(other)),
            _ => {}
        }
    }

    args
}

// ============================================================================
// Application State
// ============================================================================

/// Which panel currently has focus
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum FocusedPanel {
    #[default]
    Graph,
    RightPanel,
    Sidebar,
}

/// Saved worktree state for submodule drill-down/restore.
pub(crate) struct SavedWorktreeState {
    pub(crate) worktrees: Vec<WorktreeInfo>,
    pub(crate) selected_path: Option<PathBuf>,
}

/// Consolidated worktree state for a tab.
/// Owns the worktree metadata list, the repo cache, and the user's selection.
pub(crate) struct WorktreeState {
    /// Worktree info from git (refreshed on repo state change)
    pub(crate) worktrees: Vec<WorktreeInfo>,
    /// Opened git2::Repository handles keyed by worktree path
    pub(crate) repo_cache: HashMap<PathBuf, GitRepo>,
    /// User's currently selected worktree (None = no worktree selected)
    pub(crate) selected_path: Option<PathBuf>,
}

impl WorktreeState {
    fn new() -> Self {
        Self {
            worktrees: Vec::new(),
            repo_cache: HashMap::new(),
            selected_path: None,
        }
    }

    /// Returns the staging repo for the selected worktree, if any.
    /// This is the ONLY place worktree repo resolution happens.
    fn staging_repo(&self) -> Option<&GitRepo> {
        self.selected_path
            .as_ref()
            .and_then(|p| self.repo_cache.get(p))
    }

    /// Returns the staging repo, falling back to the ref repo.
    /// Use for operations that work on either (status, diff, stage, commit).
    fn staging_repo_or<'a>(&'a self, ref_repo: &'a GitRepo) -> &'a GitRepo {
        self.staging_repo().unwrap_or(ref_repo)
    }

    /// Select a worktree by path, opening its repo if not cached.
    fn select(&mut self, path: PathBuf) {
        if let std::collections::hash_map::Entry::Vacant(entry) =
            self.repo_cache.entry(path.clone())
            && let Ok(repo) = GitRepo::open(entry.key())
        {
            entry.insert(repo);
        }
        self.selected_path = Some(path);
    }

    /// Clear selection (no worktree selected).
    #[allow(dead_code)]
    fn deselect(&mut self) {
        self.selected_path = None;
    }

    /// Whether we have multiple worktrees (determines if selector is shown).
    fn has_selector(&self) -> bool {
        self.worktrees.len() >= 2
    }

    /// Refresh worktree list from repo and prune stale cache entries.
    /// Does NOT open worktree repos — those are opened on background threads
    /// via `spawn_repo_state_refresh` and merged in `apply_repo_state_result`.
    fn refresh(&mut self, repo: &GitRepo) {
        self.worktrees = repo.worktrees().unwrap_or_default();
        // Prune cache entries for removed worktrees
        let valid: HashSet<PathBuf> = self
            .worktrees
            .iter()
            .map(|wt| PathBuf::from(&wt.path))
            .collect();
        self.repo_cache.retain(|p, _| valid.contains(p));
    }

    /// Derive current_branch and head_oid from the selected worktree.
    /// Returns (branch, head_oid).
    fn derive_head(&self, ref_repo: &GitRepo) -> (String, Option<Oid>) {
        if let Some(staging) = self.staging_repo() {
            (
                staging.current_branch().unwrap_or_default(),
                staging.head_oid().ok(),
            )
        } else if self.has_selector() {
            // Multi-worktree, nothing selected — HEAD is meaningless
            (String::new(), None)
        } else {
            // Single-worktree / normal repo — use the repo itself
            (
                ref_repo.current_branch().unwrap_or_default(),
                ref_repo.head_oid().ok(),
            )
        }
    }

    /// Save state for submodule drill-down.
    fn save(&self) -> SavedWorktreeState {
        SavedWorktreeState {
            worktrees: self.worktrees.clone(),
            selected_path: self.selected_path.clone(),
        }
    }

    /// Restore state from submodule drill-up.
    fn restore(&mut self, saved: SavedWorktreeState, repo: &GitRepo) {
        self.worktrees = saved.worktrees;
        self.selected_path = saved.selected_path;
        self.repo_cache.clear();
        self.refresh(repo);
    }
}

pub(crate) struct SavedParentState {
    pub(crate) repo: GitRepo,
    pub(crate) commits: Vec<CommitInfo>,
    pub(crate) repo_name: String,
    pub(crate) parent_branch: String,
    pub(crate) parent_pinned_oid: Option<Oid>,
    pub(crate) parent_index_oid: Option<Oid>,
    pub(crate) parent_workdir_oid: Option<Oid>,
    pub(crate) parent_pin_branches: Vec<String>,
    pub(crate) graph_scroll_offset: f32,
    pub(crate) graph_top_row_index: usize,
    pub(crate) selected_commit: Option<Oid>,
    pub(crate) sidebar_scroll_offset: f32,
    pub(crate) submodule_name: String,
    pub(crate) parent_submodules: Vec<SubmoduleInfo>,
    pub(crate) worktree_state: SavedWorktreeState,
}

/// Focus state when viewing a submodule (supports nesting via stack)
pub(crate) struct SubmoduleFocus {
    pub(crate) parent_stack: Vec<SavedParentState>,
    pub(crate) current_name: String,
}

/// Per-tab repository data
pub(crate) struct RepoTab {
    pub(crate) id: u64,
    pub(crate) repo: GitRepo,
    pub(crate) commits: Vec<CommitInfo>,
    pub(crate) name: String,
}

/// Per-tab UI view state
pub(crate) struct TabViewState {
    pub(crate) focused_panel: FocusedPanel,
    pub(crate) right_panel_mode: RightPanelMode,
    pub(crate) header_bar: HeaderBar,
    pub(crate) shortcut_bar: ShortcutBar,
    pub(crate) branch_sidebar: BranchSidebar,
    pub(crate) commit_graph_view: CommitGraphView,
    pub(crate) staging_well: StagingWell,
    pub(crate) diff_view: DiffView,
    pub(crate) commit_detail_view: CommitDetailView,
    pub(crate) context_menu: ContextMenu,
    /// Oid of the commit that was right-clicked for context menu
    pub(crate) context_menu_commit: Option<Oid>,
    pub(crate) last_diff_commit: Option<Oid>,
    pub(crate) pending_messages: Vec<AppMessage>,
    /// Receiver for async status refresh (background thread)
    pub(crate) status_receiver: Option<Receiver<StatusResult>>,
    /// Receiver for async repo state refresh (background thread)
    pub(crate) repo_state_receiver: Option<Receiver<RepoStateResult>>,
    /// Receiver for async diff stats computation
    pub(crate) diff_stats_receiver: Option<Receiver<Vec<(Oid, usize, usize)>>>,
    pub(crate) fetch_receiver: TimedRemoteOpSlot,
    pub(crate) pull_receiver: TimedRemoteOpSlot,
    pub(crate) push_receiver: TimedRemoteOpSlot,
    /// Generic async receiver for submodule/worktree ops (label for toast)
    pub(crate) generic_op_receiver: GenericRemoteOpSlot,
    /// Track whether we already showed the "still running" toast for each op
    pub(crate) showed_timeout_toast: [bool; 4],
    /// Consolidated worktree state: metadata, repo cache, and selection
    pub(crate) worktree_state: WorktreeState,
    /// Submodule drill-down state (None when viewing root repo)
    pub(crate) submodule_focus: Option<SubmoduleFocus>,
    /// Filesystem watcher for auto-refresh on external changes
    pub(crate) watcher: Option<RepoWatcher>,
    pub(crate) watcher_rx: Option<Receiver<FsChangeKind>>,
    /// Receiver for async watcher initialization to avoid blocking the event loop.
    pub(crate) watcher_init_receiver: Option<Receiver<WatcherInitResult>>,
    /// Current branch name — derived from the active worktree's staging repo.
    /// Single source of truth; views read this instead of keeping their own copies.
    pub(crate) current_branch: String,
    /// HEAD commit OID — derived from the active worktree's staging repo.
    /// Single source of truth for the graph HEAD glow and Key::G jump.
    pub(crate) head_oid: Option<Oid>,
    /// Fingerprint of HEAD + local branch tip OIDs for periodic ref reconciliation.
    /// Compared every 5s to detect external ref changes missed by the watcher.
    pub(crate) ref_fingerprint: u64,
    /// Receivers for async CI status fetches (one per provider)
    pub(crate) ci_receivers: Vec<Receiver<ci::ProviderCiResult>>,
    /// CI results from all providers (accumulated as receivers complete)
    pub(crate) ci_results: Vec<ci::ProviderCiResult>,
    /// When CI status was last fetched (for periodic polling)
    pub(crate) last_ci_fetch: Instant,
    /// When the last push completed (enables fast CI polling for 5 min)
    pub(crate) last_push_time: Option<Instant>,
}

impl TabViewState {
    /// Returns `Some(&str)` when current_branch is non-empty, `None` otherwise.
    fn current_branch_opt(&self) -> Option<&str> {
        if self.current_branch.is_empty() {
            None
        } else {
            Some(&self.current_branch)
        }
    }

    /// Re-derive current_branch and head_oid from worktree state,
    /// and update branch_tips[].is_head accordingly.
    fn sync_worktree_derived_state(&mut self, repo: &GitRepo) {
        let (branch, head) = self.worktree_state.derive_head(repo);
        self.current_branch = branch;
        self.head_oid = head;
        for tip in &mut self.commit_graph_view.branch_tips {
            tip.is_head = !tip.is_remote && tip.name == self.current_branch;
        }
    }

    /// Switch the staging well to a different worktree by index.
    /// Dismisses any commit inspect activity and enters staging mode.
    fn switch_to_worktree(&mut self, index: usize, repo: &GitRepo) {
        self.staging_well.switch_worktree(index);
        self.right_panel_mode = RightPanelMode::Staging;
        self.diff_view.clear();
        self.commit_detail_view.clear();
        self.commit_graph_view.selected_commit = None;
        self.last_diff_commit = None;
        if let Some(path) = self.staging_well.active_worktree_path() {
            self.worktree_state.select(path);
        }
        self.sync_worktree_derived_state(repo);
    }

    /// Switch to a named worktree (looks up index by name).
    fn switch_to_worktree_by_name(&mut self, name: &str, repo: &GitRepo) {
        if let Some(idx) = self.staging_well.worktree_index_by_name(name) {
            self.switch_to_worktree(idx, repo);
        }
    }

    /// Handle a staging action by dispatching to the appropriate pending message.
    fn handle_staging_action(&mut self, action: StagingAction, repo: &GitRepo) {
        match action {
            StagingAction::StageFile(path) => {
                self.pending_messages.push(AppMessage::StageFile(path));
            }
            StagingAction::UnstageFile(path) => {
                self.pending_messages.push(AppMessage::UnstageFile(path));
            }
            StagingAction::StageFiles(paths) => {
                for path in paths {
                    self.pending_messages.push(AppMessage::StageFile(path));
                }
            }
            StagingAction::UnstageFiles(paths) => {
                for path in paths {
                    self.pending_messages.push(AppMessage::UnstageFile(path));
                }
            }
            StagingAction::StageAll => {
                self.pending_messages.push(AppMessage::StageAll);
            }
            StagingAction::StageAllUntracked => {
                self.pending_messages.push(AppMessage::StageAllUntracked);
            }
            StagingAction::UnstageAll => {
                self.pending_messages.push(AppMessage::UnstageAll);
            }
            StagingAction::Commit(message) => {
                self.pending_messages.push(AppMessage::Commit(message));
            }
            StagingAction::AmendCommit(message) => {
                self.pending_messages.push(AppMessage::AmendCommit(message));
            }
            StagingAction::ToggleAmend => {
                self.pending_messages.push(AppMessage::ToggleAmend);
            }
            StagingAction::ViewDiff(path) => {
                let staged = self
                    .staging_well
                    .staged_list
                    .files
                    .iter()
                    .any(|f| f.path == path);
                self.pending_messages
                    .push(AppMessage::ViewDiff(path, staged));
            }
            StagingAction::SwitchWorktree(index) => {
                self.switch_to_worktree(index, repo);
            }
            StagingAction::PreviewDiff(path, staged) => {
                self.pending_messages
                    .push(AppMessage::ViewDiff(path, staged));
            }
            StagingAction::OpenSubmodule(name) => {
                self.pending_messages.push(AppMessage::EnterSubmodule(name));
            }
            StagingAction::GenerateAiCommitMessage => {
                self.pending_messages
                    .push(AppMessage::AiGenerateCommitMessage);
            }
        }
    }

    /// Handle a graph action by dispatching to the appropriate pending message.
    fn handle_graph_action(&mut self, action: GraphAction, repo: &GitRepo) {
        match action {
            GraphAction::LoadMore => {
                self.pending_messages.push(AppMessage::LoadMoreCommits);
            }
            GraphAction::SwitchWorktree(name) => {
                self.switch_to_worktree_by_name(&name, repo);
            }
        }
    }

    fn new() -> Self {
        Self {
            focused_panel: FocusedPanel::Graph,
            right_panel_mode: RightPanelMode::Staging,
            header_bar: HeaderBar::new(),
            shortcut_bar: ShortcutBar::new(),
            branch_sidebar: BranchSidebar::new(),
            commit_graph_view: CommitGraphView::new(),
            staging_well: StagingWell::new(),
            diff_view: DiffView::new(),
            commit_detail_view: CommitDetailView::new(),
            context_menu: ContextMenu::new(),
            context_menu_commit: None,
            last_diff_commit: None,
            pending_messages: Vec::new(),
            status_receiver: None,
            repo_state_receiver: None,
            diff_stats_receiver: None,
            fetch_receiver: None,
            pull_receiver: None,
            push_receiver: None,
            generic_op_receiver: None,
            showed_timeout_toast: [false; 4],
            worktree_state: WorktreeState::new(),
            submodule_focus: None,
            watcher: None,
            watcher_rx: None,
            watcher_init_receiver: None,
            current_branch: String::new(),
            head_oid: None,
            ref_fingerprint: 0,
            ci_receivers: Vec::new(),
            ci_results: Vec::new(),
            last_ci_fetch: Instant::now() - Duration::from_secs(600),
            last_push_time: None,
        }
    }
}

// ============================================================================
// Application
// ============================================================================

fn main() -> Result<()> {
    crash_log::init();
    crash_log::install_panic_hook();

    let cli_args = parse_args();

    let event_loop = EventLoop::new().context("Failed to create event loop")?;

    let proxy = event_loop.create_proxy();
    let mut app = App::new(cli_args, proxy)?;

    // run_app may return an error on Wayland if the compositor disconnects
    // (e.g. broken pipe during flush). Treat this as a clean exit rather than
    // propagating a confusing error to the user.
    if let Err(e) = event_loop.run_app(&mut app) {
        let since_frame = app.last_completed_frame.elapsed();
        let since_start = app.app_start.elapsed();
        eprintln!("Event loop exited: {e}");
        eprintln!(
            "  Time since last completed frame: {:.3}s (app uptime: {:.1}s)",
            since_frame.as_secs_f64(),
            since_start.as_secs_f64(),
        );
        eprintln!("  Tabs: {} (active: {})", app.tabs.len(), app.active_tab);
        let status_in_flight = app
            .tabs
            .iter()
            .filter(|(_, vs)| vs.status_receiver.is_some())
            .count();
        let repo_state_in_flight = app
            .tabs
            .iter()
            .filter(|(_, vs)| vs.repo_state_receiver.is_some())
            .count();
        let diff_stats_in_flight = app
            .tabs
            .iter()
            .filter(|(_, vs)| vs.diff_stats_receiver.is_some())
            .count();
        eprintln!(
            "  In-flight: repo_state_tabs={} status_tabs={} diff_stats_tabs={} open={} clone={} text_rebuild={}",
            repo_state_in_flight,
            status_in_flight,
            diff_stats_in_flight,
            app.open_receiver.is_some(),
            app.clone_receiver.is_some(),
            app.text_rebuild_receiver.is_some(),
        );
        eprintln!("  Consecutive draw errors: {}", app.consecutive_draw_errors);
        if let Some(state) = &app.state {
            eprintln!("  Frame count: {}", state.frame_count);
            eprintln!(
                "  Swapchain needs_recreate: {}",
                state.surface.needs_recreate
            );
        }
        if let Some((_, vs)) = app.tabs.get(app.active_tab) {
            eprintln!(
                "  Active tab: ci_receivers={} generic_op={} watcher={}",
                vs.ci_receivers.len(),
                vs.generic_op_receiver.is_some(),
                vs.watcher.is_some(),
            );
        }
    }

    Ok(())
}

/// Which divider is currently being dragged
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DividerDrag {
    /// Vertical divider between sidebar and graph
    SidebarGraph,
    /// Vertical divider between graph and right panel
    GraphRight,
    /// Horizontal divider between staging and preview within the right panel
    StagingPreview,
}

/// Which modal dialog is currently receiving events.
/// Only one modal is active at a time. Rendering uses dialog `visible` flags independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActiveModal {
    Settings,
    TokenManager,
    Confirm,
    Error,
    BranchName,
    Remote,
    Pull,
    Push,
    Merge,
    Rebase,
    RepoDialog,
    CloneDialog,
}

pub(crate) struct App {
    pub(crate) cli_args: CliArgs,
    pub(crate) config: Config,
    pub(crate) tabs: Vec<(RepoTab, TabViewState)>,
    /// Monotonic ID source for new tabs (stable across index shifts/removals).
    pub(crate) next_tab_id: u64,
    pub(crate) active_tab: usize,
    pub(crate) tab_bar: TabBar,
    pub(crate) repo_dialog: RepoDialog,
    pub(crate) clone_dialog: CloneDialog,
    pub(crate) settings_dialog: SettingsDialog,
    pub(crate) token_dialog: TokenDialog,
    pub(crate) confirm_dialog: ConfirmDialog,
    pub(crate) error_dialog: ErrorDialog,
    pub(crate) branch_name_dialog: BranchNameDialog,
    pub(crate) remote_dialog: RemoteDialog,
    pub(crate) merge_dialog: MergeDialog,
    pub(crate) rebase_dialog: RebaseDialog,
    pub(crate) pull_dialog: PullDialog,
    pub(crate) push_dialog: PushDialog,
    /// Which modal dialog is active (receives events). None = no modal open.
    pub(crate) active_modal: Option<ActiveModal>,
    /// Modal that was active before an interrupt (Error/Confirm) opened.
    /// Restored when the interrupt modal closes.
    pub(crate) interrupted_modal: Option<ActiveModal>,
    pub(crate) pending_confirm_action: Option<AppMessage>,
    pub(crate) toast_manager: ToastManager,
    pub(crate) tooltip: Tooltip,
    pub(crate) state: Option<RenderState>,
    /// Which divider is currently being dragged, if any
    pub(crate) divider_drag: Option<DividerDrag>,
    /// Fraction of total width for sidebar (default ~0.14)
    pub(crate) sidebar_ratio: f32,
    /// Fraction of content width (after sidebar) for graph (default 0.55)
    pub(crate) graph_ratio: f32,
    /// Whether the shortcut bar is visible
    pub(crate) shortcut_bar_visible: bool,
    /// Fraction of right panel height for staging (default 0.45), remainder is preview
    pub(crate) staging_preview_ratio: f32,
    /// Current cursor icon (cached to avoid redundant Wayland protocol calls)
    pub(crate) current_cursor: CursorIcon,
    /// Dirty flag: true when refresh_status() should run on next frame
    pub(crate) status_dirty: bool,
    /// Timestamp of last refresh_status() call, for periodic refresh
    pub(crate) last_status_refresh: Instant,
    /// Timestamp of app creation, for animation elapsed time
    pub(crate) app_start: Instant,
    /// Receiver for async AI commit message generation
    pub(crate) ai_commit_receiver: Option<(Receiver<Result<ai::AiResponse, String>>, Instant)>,
    /// AI provider resolved from config at startup
    pub(crate) ai_provider: ai::AiProvider,
    /// Proxy to wake the event loop from background threads
    pub(crate) proxy: EventLoopProxy<()>,
    /// Timestamp of the last frame render, for animation scheduling
    pub(crate) last_frame_time: Instant,
    /// Timestamp of the last successfully completed RedrawRequested cycle.
    /// Used to report how long the main thread was unresponsive on crash.
    pub(crate) last_completed_frame: Instant,
    /// Count of consecutive draw_frame errors (reset to 0 on success).
    pub(crate) consecutive_draw_errors: u32,
    /// Timestamp of the last periodic ref fingerprint check
    pub(crate) last_ref_check: Instant,
    /// Receiver for async git clone operation (success = destination path)
    pub(crate) clone_receiver: Option<(Receiver<Result<PathBuf, String>>, Instant)>,
    /// Receiver for async repo open operation (background GitRepo::open)
    pub(crate) open_receiver: Option<Receiver<Result<(GitRepo, String), String>>>,
    /// "Before" snapshot for async diagnostic reload (F5). When Some, a diagnostic
    /// reload is in progress; finalized when both repo state and status results arrive.
    pub(crate) diagnostic_before: Option<RepoStateSnapshot>,
    /// Receiver for async text renderer rebuild (HiDPI monitor switch)
    pub(crate) text_rebuild_receiver: Option<Receiver<(TextRenderer, TextRenderer)>>,
    /// Timestamp of last window resize event, for debouncing swapchain recreation.
    /// While set, the swapchain is rendered at its existing size (the compositor
    /// scales it) to avoid expensive per-frame swapchain + MSAA reallocation
    /// during compositor-animated resizes (e.g. KDE tile/snap).
    pub(crate) resize_debounce: Option<Instant>,
    /// Persistent channel for per-entity dirty check results (submodule/worktree).
    /// Results carry `tab_id` and are routed to the matching tab, so updates
    /// remain correct even when tabs are switched or closed mid-flight.
    /// The sender is cloned to each spawned dirty check thread.
    pub(crate) dirty_check_tx: std::sync::mpsc::Sender<async_polling::DirtyCheckResult>,
    pub(crate) dirty_check_rx: std::sync::mpsc::Receiver<async_polling::DirtyCheckResult>,
    /// Number of dirty checks currently in flight (decremented as results arrive).
    pub(crate) dirty_checks_in_flight: usize,
}

/// Initialized render state (after window creation) - shared across all tabs
pub(crate) struct RenderState {
    pub(crate) window: Arc<Window>,
    pub(crate) ctx: VulkanContext,
    pub(crate) surface: SurfaceManager,
    pub(crate) text_renderer: TextRenderer,
    pub(crate) bold_text_renderer: TextRenderer,
    pub(crate) spline_renderer: SplineRenderer,
    pub(crate) avatar_renderer: AvatarRenderer,
    pub(crate) avatar_cache: AvatarCache,
    pub(crate) icon_renderer: IconRenderer,
    pub(crate) previous_frame_end: Option<Box<dyn GpuFuture>>,
    pub(crate) frame_count: u32,
    pub(crate) scale_factor: f64,
    pub(crate) input_state: InputState,
}

/// Build new text renderers on the current thread (CPU-heavy: rasterization, SDF, kerning).
/// Returns the pair ready for use. Called from a background thread to avoid blocking the event loop.
fn build_text_renderers(
    command_buffer_allocator: Arc<StandardCommandBufferAllocator>,
    queue: Arc<vulkano::device::Queue>,
    memory_allocator: Arc<StandardMemoryAllocator>,
    render_pass: Arc<vulkano::render_pass::RenderPass>,
    device: Arc<vulkano::device::Device>,
    atlas_build_scale: f64,
    display_scale: f64,
) -> Result<(TextRenderer, TextRenderer)> {
    let mut upload_builder = AutoCommandBufferBuilder::primary(
        command_buffer_allocator,
        queue.queue_family_index(),
        CommandBufferUsage::OneTimeSubmit,
    )
    .context("Failed to create text upload command buffer")?;

    let mut text_renderer = TextRenderer::new(
        memory_allocator.clone(),
        render_pass.clone(),
        &mut upload_builder,
        atlas_build_scale,
    )
    .context("Failed to rebuild text renderer")?;

    let mut bold_text_renderer = TextRenderer::new_bold(
        memory_allocator,
        render_pass,
        &mut upload_builder,
        atlas_build_scale,
    )
    .context("Failed to rebuild bold text renderer")?;

    text_renderer.set_render_scale(display_scale);
    bold_text_renderer.set_render_scale(display_scale);

    let upload_cb = upload_builder
        .build()
        .context("Failed to build text upload command buffer")?;
    let upload_future = sync::now(device)
        .then_execute(queue, upload_cb)
        .context("Failed to execute text upload")?
        .then_signal_fence_and_flush()
        .map_err(Validated::unwrap)
        .context("Failed to flush text upload")?;
    upload_future
        .wait(None)
        .context("Failed to wait for text upload")?;

    Ok((text_renderer, bold_text_renderer))
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_none()
            && let Err(e) = self.init_state(event_loop)
        {
            eprintln!("Failed to initialize: {e:?}");
            event_loop.exit();
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
        // Background thread completed — request redraw to poll results
        if let Some(state) = &self.state {
            state.window.request_redraw();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.state.is_none() {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                crash_log::mark_clean_exit();
                event_loop.exit();
            }

            WindowEvent::Resized(_) => self.handle_window_resized(),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.handle_scale_factor_changed(scale_factor);
            }
            WindowEvent::RedrawRequested => self.handle_redraw_requested(event_loop),
            WindowEvent::DroppedFile(ref path) => self.handle_dropped_file(path.clone()),
            ref win_event => self.handle_input_window_event(event_loop, win_event),
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(state) = &self.state else { return };
        let now = Instant::now();

        match self.next_wake_deadline(now) {
            Some(t) if t <= now => {
                // Timer already due — draw immediately
                state.window.request_redraw();
            }
            Some(t) => {
                event_loop.set_control_flow(ControlFlow::WaitUntil(t));
            }
            None => {
                event_loop.set_control_flow(ControlFlow::Wait);
            }
        }
    }
}
