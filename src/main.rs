//! Main application entry point and event loop.
//!
//! Owns the App struct, winit event loop, Vulkan draw pipeline, and three-layer rendering architecture
//! (base → chrome → overlay). Handles async git operations via mpsc channels and thread spawning.

mod ai;
mod app_async_polling;
mod app_input_handlers;
mod app_modal_state;
mod app_repo_lifecycle;
mod app_scheduling;
mod app_settings_tokens;
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

impl App {
    fn new(cli_args: CliArgs, proxy: EventLoopProxy<()>) -> Result<Self> {
        let mut config = Config::load();
        let mut tabs = Vec::new();
        let mut tab_bar = TabBar::new();
        let mut next_tab_id = 1_u64;

        // Determine repo paths to open
        let repo_paths: Vec<PathBuf> = if cli_args.repos.is_empty() {
            vec![PathBuf::from(".")]
        } else {
            cli_args.repos.clone()
        };

        for repo_path in &repo_paths {
            match GitRepo::open(repo_path) {
                Ok(repo) => {
                    let name = repo.repo_name();
                    tab_bar.add_tab(name.clone());
                    tabs.push((
                        RepoTab {
                            id: next_tab_id,
                            repo,
                            commits: Vec::new(),
                            name,
                        },
                        TabViewState::new(),
                    ));
                    next_tab_id = next_tab_id.saturating_add(1);
                }
                Err(e) => {
                    eprintln!("Warning: Could not open repository at {:?}: {e}", repo_path);
                    // Skip creating a tab for failed repos
                }
            }
        }
        // Ensure at least one tab exists (even if all repos failed to open)
        if tabs.is_empty() {
            anyhow::bail!("No repositories could be opened");
        }

        let mut settings_dialog = SettingsDialog::new();
        settings_dialog.show_avatars = config.avatars_enabled;
        settings_dialog.scroll_speed = if config.fast_scroll { 2.0 } else { 1.0 };
        settings_dialog.row_scale = config.row_scale;
        settings_dialog.abbreviate_worktree_names = config.abbreviate_worktree_names;
        settings_dialog.time_spacing_strength = config.time_spacing_strength;
        settings_dialog.show_orphaned_commits = config.show_orphaned_commits;
        settings_dialog.ratchet_scroll = config.ratchet_scroll;
        let shortcut_bar_visible = config.shortcut_bar_visible;
        let ai_provider = ai::AiProvider::from_config(&config.ai_provider);
        let token_dialog = TokenDialog::new();

        // Migrate plaintext tokens from config to system keychain
        {
            let has_plaintext = config.github_token.is_some() || !config.gitlab_tokens.is_empty();
            if has_plaintext && token_store::is_available() {
                let (migrated, errors) = token_store::migrate_from_config(
                    config.github_token.as_deref(),
                    &config.gitlab_tokens,
                );
                if migrated > 0 {
                    // Clear plaintext token values but keep host keys as registry
                    config.github_token = None;
                    for value in config.gitlab_tokens.values_mut() {
                        *value = String::new();
                    }
                    let _ = config.save();
                }
                for err in errors {
                    eprintln!("Token migration warning: {err}");
                }
            }
        }

        let (dirty_check_tx, dirty_check_rx) = std::sync::mpsc::channel();
        Ok(Self {
            cli_args,
            config,
            tabs,
            next_tab_id,
            active_tab: 0,
            tab_bar,
            repo_dialog: RepoDialog::new(),
            clone_dialog: CloneDialog::new(),
            settings_dialog,
            token_dialog,
            confirm_dialog: ConfirmDialog::new(),
            error_dialog: ErrorDialog::new(),
            branch_name_dialog: BranchNameDialog::new(),
            remote_dialog: RemoteDialog::new(),
            merge_dialog: MergeDialog::new(),
            rebase_dialog: RebaseDialog::new(),
            pull_dialog: PullDialog::new(),
            push_dialog: PushDialog::new(),
            active_modal: None,
            interrupted_modal: None,
            pending_confirm_action: None,
            toast_manager: ToastManager::new(),
            tooltip: Tooltip::new(),
            state: None,
            divider_drag: None,
            sidebar_ratio: 0.14,
            graph_ratio: 0.55,
            staging_preview_ratio: 0.40,
            shortcut_bar_visible,
            current_cursor: CursorIcon::Default,
            status_dirty: true,
            last_status_refresh: Instant::now(),
            app_start: Instant::now(),
            ai_commit_receiver: None,
            ai_provider,
            proxy,
            last_frame_time: Instant::now(),
            last_completed_frame: Instant::now(),
            consecutive_draw_errors: 0,
            last_ref_check: Instant::now(),
            clone_receiver: None,
            open_receiver: None,
            diagnostic_before: None,
            text_rebuild_receiver: None,
            resize_debounce: None,
            dirty_check_tx,
            dirty_check_rx,
            dirty_checks_in_flight: 0,
        })
    }

    fn init_state(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        // Create window
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Whisper Git")
                        .with_inner_size(winit::dpi::LogicalSize::new(1600, 900)),
                )
                .context("Failed to create window")?,
        );

        // Enable IME so we receive Ime::Commit events for text input
        window.set_ime_allowed(true);

        // Create Vulkan context (needs surface for device selection)
        let library = vulkano::VulkanLibrary::new().context("No Vulkan library")?;
        let required_extensions = vulkano::swapchain::Surface::required_extensions(event_loop)
            .context("Failed to get extensions")?;
        let instance = vulkano::instance::Instance::new(
            library,
            vulkano::instance::InstanceCreateInfo {
                enabled_extensions: required_extensions,
                ..Default::default()
            },
        )
        .context("Failed to create instance")?;

        let surface = vulkano::swapchain::Surface::from_window(instance.clone(), window.clone())
            .context("Failed to create surface")?;

        let ctx = VulkanContext::with_surface(instance, &surface)?;
        crash_log::set_vulkan_device(&ctx.device.physical_device().properties().device_name);

        // Create render pass with MSAA 4x.
        // The resolve target's final_layout is PresentSrc so the swapchain
        // image is in the spec-required layout for vkQueuePresentKHR.
        // Vulkano's then_swapchain_present() does not insert this transition
        // (upstream TODO in acquire_present.rs), so without this override the
        // image would be presented in TransferDstOptimal — which causes
        // corruption on some drivers, particularly multi-GPU X11 with
        // DRI3/PRIME.
        let image_format = SurfaceManager::choose_surface_format(&ctx, &surface)?;
        let render_pass = vulkano::single_pass_renderpass!(
            ctx.device.clone(),
            attachments: {
                msaa_color: {
                    format: image_format,
                    samples: 4,
                    load_op: Clear,
                    store_op: DontCare,
                },
                resolve_target: {
                    format: image_format,
                    samples: 1,
                    load_op: DontCare,
                    store_op: Store,
                    final_layout: ImageLayout::PresentSrc,
                },
            },
            pass: {
                color: [msaa_color],
                color_resolve: [resolve_target],
                depth_stencil: {},
            },
        )
        .context("Failed to create render pass")?;

        // Create surface manager (reuse the surface from device selection)
        let surface_mgr =
            SurfaceManager::new(&ctx, &surface, window.inner_size(), render_pass.clone())?;

        // Create text renderer
        let mut upload_builder = AutoCommandBufferBuilder::primary(
            ctx.command_buffer_allocator.clone(),
            ctx.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .context("Failed to create upload command buffer")?;

        // Build font atlas close to the active window scale so low-DPI displays
        // don't aggressively minify oversized glyph atlases.
        // CLI --scale still overrides this for deterministic screenshots.
        let window_scale = window.scale_factor();
        let atlas_build_scale = self.cli_args.screenshot_scale.unwrap_or(window_scale);
        let mut text_renderer = TextRenderer::new(
            ctx.memory_allocator.clone(),
            render_pass.clone(),
            &mut upload_builder,
            atlas_build_scale,
        )
        .context("Failed to create text renderer")?;

        let mut bold_text_renderer = TextRenderer::new_bold(
            ctx.memory_allocator.clone(),
            render_pass.clone(),
            &mut upload_builder,
            atlas_build_scale,
        )
        .context("Failed to create bold text renderer")?;
        text_renderer.set_render_scale(window_scale);
        bold_text_renderer.set_render_scale(window_scale);

        if std::env::var_os("WHISPER_TEXT_DIAG").is_some() {
            let max_monitor_scale = window
                .available_monitors()
                .map(|m| m.scale_factor())
                .fold(window_scale, f64::max);
            eprintln!(
                "text_diag init: window_scale={window_scale:.2} atlas_build_scale={atlas_build_scale:.2} max_monitor_scale={max_monitor_scale:.2}"
            );
        }

        let spline_renderer =
            SplineRenderer::new(ctx.memory_allocator.clone(), render_pass.clone())
                .context("Failed to create spline renderer")?;

        let avatar_renderer =
            AvatarRenderer::new(ctx.memory_allocator.clone(), render_pass.clone())
                .context("Failed to create avatar renderer")?;

        let mut icon_renderer =
            IconRenderer::new(ctx.memory_allocator.clone(), render_pass.clone())
                .context("Failed to create icon renderer")?;
        crate::ui::icon::register_builtin_icons(&mut icon_renderer);

        // Submit font atlas upload
        let upload_buffer = upload_builder
            .build()
            .context("Failed to build upload buffer")?;
        let upload_future = sync::now(ctx.device.clone())
            .then_execute(ctx.queue.clone(), upload_buffer)
            .context("Failed to execute upload")?
            .then_signal_fence_and_flush()
            .map_err(Validated::unwrap)
            .context("Failed to flush upload")?;
        upload_future
            .wait(None)
            .context("Failed to wait for upload")?;

        let previous_frame_end = Some(sync::now(ctx.device.clone()).boxed());

        // Initialize all tab views
        let scale = window_scale as f32;
        let row_scale = self.settings_dialog.row_scale;
        let abbreviate_wt = self.settings_dialog.abbreviate_worktree_names;
        let time_strength = self.settings_dialog.time_spacing_strength;
        let fast_scroll = self.config.fast_scroll;
        for (repo_tab, view_state) in &mut self.tabs {
            view_state.commit_graph_view.row_scale = row_scale;
            view_state.commit_graph_view.abbreviate_worktree_names = abbreviate_wt;
            view_state.commit_graph_view.time_spacing_strength = time_strength;
            view_state.commit_graph_view.fast_scroll = fast_scroll;
            view_state.commit_graph_view.ratchet_scroll = self.config.ratchet_scroll;
            view_state.repo_state_receiver = Some(init_tab_view(
                repo_tab,
                view_state,
                &text_renderer,
                scale,
                self.config.show_orphaned_commits,
                &self.proxy,
            ));
        }

        self.state = Some(RenderState {
            window,
            ctx,
            surface: surface_mgr,
            text_renderer,
            bold_text_renderer,
            spline_renderer,
            avatar_renderer,
            avatar_cache: AvatarCache::new(self.proxy.clone()),
            icon_renderer,
            previous_frame_end,
            frame_count: 0,
            scale_factor: window_scale,
            input_state: InputState::new(),
        });

        // Set event loop proxy for file picker wake-up
        self.repo_dialog.set_proxy(self.proxy.clone());
        self.clone_dialog.set_proxy(self.proxy.clone());

        // Initial status refresh for active tab
        self.refresh_status();

        // Crash log housekeeping
        crash_log::prune_crash_logs(10);
        if let Some(path) = crash_log::has_crash_since_last_exit() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            self.toast_manager.push(
                format!("Whisper-Git crashed last session. Log: {}", name),
                ToastSeverity::Info,
            );
        }
        crash_log::breadcrumb("init_state complete".to_string());

        // Ensure first frame is drawn immediately (don't rely on platform Resized event)
        if let Some(state) = &self.state {
            state.window.request_redraw();
        }

        Ok(())
    }

    /// Diagnostic reload: capture current UI state, kick off async re-read.
    /// The "after" snapshot and delta report are produced when the background
    /// results arrive (see `finalize_diagnostic_reload`).
    fn do_diagnostic_reload(&mut self) {
        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };

        // 1. Capture "before" snapshot from current UI state
        let before = {
            let msg_view = MessageViewState {
                commit_graph_view: &mut view_state.commit_graph_view,
                staging_well: &mut view_state.staging_well,
                diff_view: &mut view_state.diff_view,
                commit_detail_view: &mut view_state.commit_detail_view,
                branch_sidebar: &mut view_state.branch_sidebar,
                header_bar: &mut view_state.header_bar,
                last_diff_commit: &mut view_state.last_diff_commit,
                fetch_receiver: &mut view_state.fetch_receiver,
                pull_receiver: &mut view_state.pull_receiver,
                push_receiver: &mut view_state.push_receiver,
                generic_op_receiver: &mut view_state.generic_op_receiver,
                right_panel_mode: &mut view_state.right_panel_mode,
                worktrees: &mut view_state.worktree_state.worktrees,
                proxy: self.proxy.clone(),
                needs_repo_refresh: false,
            };
            RepoStateSnapshot::from_ui(
                &repo_tab.commits,
                &msg_view,
                &view_state.current_branch,
                view_state.head_oid,
            )
        };

        // 2. Reopen repos to bypass libgit2 cache
        let _ = repo_tab.repo.reopen();
        for wt_repo in view_state.worktree_state.repo_cache.values_mut() {
            let _ = wt_repo.reopen();
        }

        // 3. Store "before" snapshot and kick off async refresh
        self.diagnostic_before = Some(before);
        self.trigger_repo_state_refresh();
        self.status_dirty = true;
    }

    /// Finalize a diagnostic reload once both repo state and status results have
    /// been applied. Captures the "after" snapshot, computes deltas, writes report.
    fn finalize_diagnostic_reload(&mut self) {
        // Only finalize when both async results have been consumed
        let active_pending = self
            .tabs
            .get(self.active_tab)
            .is_some_and(|(_, view_state)| {
                view_state.repo_state_receiver.is_some() || view_state.status_receiver.is_some()
            });
        if active_pending {
            return;
        }
        let Some(before) = self.diagnostic_before.take() else {
            return;
        };
        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };

        // Capture "after" snapshot
        let after = {
            let msg_view = MessageViewState {
                commit_graph_view: &mut view_state.commit_graph_view,
                staging_well: &mut view_state.staging_well,
                diff_view: &mut view_state.diff_view,
                commit_detail_view: &mut view_state.commit_detail_view,
                branch_sidebar: &mut view_state.branch_sidebar,
                header_bar: &mut view_state.header_bar,
                last_diff_commit: &mut view_state.last_diff_commit,
                fetch_receiver: &mut view_state.fetch_receiver,
                pull_receiver: &mut view_state.pull_receiver,
                push_receiver: &mut view_state.push_receiver,
                generic_op_receiver: &mut view_state.generic_op_receiver,
                right_panel_mode: &mut view_state.right_panel_mode,
                worktrees: &mut view_state.worktree_state.worktrees,
                proxy: self.proxy.clone(),
                needs_repo_refresh: false,
            };
            RepoStateSnapshot::from_ui(
                &repo_tab.commits,
                &msg_view,
                &view_state.current_branch,
                view_state.head_oid,
            )
        };

        // Compare and write report to file
        let deltas = compute_reload_deltas(&before, &after);
        let report_dir = std::env::var("HOME")
            .map(|h| {
                std::path::PathBuf::from(h)
                    .join(".config")
                    .join("whisper-git")
                    .join("reload-reports")
            })
            .ok();
        if let Some(ref dir) = report_dir {
            let _ = std::fs::create_dir_all(dir);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let path = dir.join(format!("reload-{}.txt", now));
            let mut report = String::new();
            report.push_str(&format!("Reload report — unix {}\n", now));
            if let Some((repo_tab, _)) = self.tabs.get(self.active_tab) {
                report.push_str(&format!("Repo: {}\n", repo_tab.name));
            }
            report.push_str(&format!("Deltas: {}\n\n", deltas.len()));
            if deltas.is_empty() {
                report.push_str("No deltas detected.\n");
            } else {
                for delta in &deltas {
                    report.push_str(delta);
                    report.push('\n');
                }
            }
            report.push_str("\n--- Before snapshot ---\n");
            report.push_str(&format!(
                "Commits: {} (non-synthetic)\n",
                before.commit_oids.len()
            ));
            report.push_str(&format!("HEAD: {:?}\n", before.head_oid));
            report.push_str(&format!("Branch: {}\n", before.current_branch));
            report.push_str(&format!("Branch tips: {}\n", before.branch_tips.len()));
            report.push_str(&format!("Tags: {}\n", before.tags.len()));
            report.push_str(&format!(
                "Staged/Unstaged/Conflicted: {}/{}/{}\n",
                before.staged_count, before.unstaged_count, before.conflicted_count
            ));
            report.push_str("\n--- After snapshot ---\n");
            report.push_str(&format!(
                "Commits: {} (non-synthetic)\n",
                after.commit_oids.len()
            ));
            report.push_str(&format!("HEAD: {:?}\n", after.head_oid));
            report.push_str(&format!("Branch: {}\n", after.current_branch));
            report.push_str(&format!("Branch tips: {}\n", after.branch_tips.len()));
            report.push_str(&format!("Tags: {}\n", after.tags.len()));
            report.push_str(&format!(
                "Staged/Unstaged/Conflicted: {}/{}/{}\n",
                after.staged_count, after.unstaged_count, after.conflicted_count
            ));

            match std::fs::write(&path, &report) {
                Ok(()) => {
                    let summary = if deltas.is_empty() {
                        "Reload: no deltas".to_string()
                    } else {
                        format!(
                            "Reload: {} delta{}",
                            deltas.len(),
                            if deltas.len() == 1 { "" } else { "s" }
                        )
                    };
                    self.toast_manager.push(
                        format!("{} — {}", summary, path.display()),
                        if deltas.is_empty() {
                            ToastSeverity::Success
                        } else {
                            ToastSeverity::Info
                        },
                    );
                }
                Err(e) => {
                    self.toast_manager.push(
                        format!("Failed to write reload report: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }

        self.status_dirty = true;
    }

    fn process_messages(&mut self) {
        let tab_count = self.tabs.len();
        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };

        // Extract messages to avoid borrow conflicts
        let messages: Vec<_> = view_state.pending_messages.drain(..).collect();

        if messages.is_empty() {
            return;
        }
        crash_log::breadcrumb(format!("process_messages: {} pending", messages.len()));

        // Partition: submodule navigation vs normal messages
        let (nav_messages, mut normal_messages): (Vec<_>, Vec<_>) =
            messages.into_iter().partition(|msg| {
                matches!(
                    msg,
                    AppMessage::EnterSubmodule(_)
                        | AppMessage::ExitSubmodule
                        | AppMessage::ExitToDepth(_)
                )
            });

        // Handle ShowPullDialog and ShowPushDialog separately (need to access dialogs)
        normal_messages.retain(|msg| {
            if let AppMessage::ShowPullDialog(branch) = msg {
                let repo = &repo_tab.repo;
                let default_remote = repo
                    .default_remote()
                    .unwrap_or_else(|_| "origin".to_string());
                let remote_names = repo.remote_names();
                self.pull_dialog.show(branch, &default_remote, remote_names);
                self.active_modal = Some(ActiveModal::Pull);
                false
            } else if let AppMessage::ShowPushDialog(branch) = msg {
                let repo = &repo_tab.repo;
                let default_remote = repo
                    .default_remote()
                    .unwrap_or_else(|_| "origin".to_string());
                let remote_names = repo.remote_names();
                self.push_dialog.show(branch, &default_remote, remote_names);
                self.active_modal = Some(ActiveModal::Push);
                false
            } else if matches!(msg, AppMessage::AiGenerateCommitMessage) {
                // Inline AI generation (can't call method due to borrow conflict with repo_tab/view_state)
                if self.ai_commit_receiver.is_some() {
                    self.toast_manager.push(
                        "AI generation already in progress".to_string(),
                        ToastSeverity::Info,
                    );
                } else if view_state.staging_well.staged_list.files.is_empty() {
                    self.toast_manager.push(
                        "No staged files — stage changes first".to_string(),
                        ToastSeverity::Info,
                    );
                } else {
                    let repo = &repo_tab.repo;
                    let staging_repo = view_state.worktree_state.staging_repo_or(repo);
                    match staging_repo.staged_diff_text(50_000) {
                        Ok(diff_text) if diff_text.trim().is_empty() => {
                            self.toast_manager
                                .push("Staged diff is empty".to_string(), ToastSeverity::Info);
                        }
                        Ok(diff_text) => {
                            let branch = view_state.current_branch.clone();
                            let provider = self.ai_provider.clone();
                            let provider_name = provider.display_name().to_string();
                            let (tx, rx) = std::sync::mpsc::channel();
                            let ai_proxy = self.proxy.clone();
                            std::thread::spawn(move || {
                                let request = ai::AiRequest { diff_text, branch };
                                let result = provider.generate_commit_message(&request);
                                let _ = tx.send(result);
                                let _ = ai_proxy.send_event(());
                            });
                            self.ai_commit_receiver = Some((rx, Instant::now()));
                            view_state.staging_well.ai_generating = true;
                            self.toast_manager.push(
                                format!("Generating commit message via {}...", provider_name),
                                ToastSeverity::Info,
                            );
                        }
                        Err(e) => {
                            self.toast_manager.push(
                                format!("Failed to read staged diff: {}", e),
                                ToastSeverity::Error,
                            );
                        }
                    }
                }
                false
            } else {
                true
            }
        });

        // Handle submodule navigation first (needs text_renderer from self.state)
        if !nav_messages.is_empty() {
            let mut nav_rx: Option<Receiver<RepoStateResult>> = None;
            if let Some(ref state) = self.state {
                let scale = state.scale_factor as f32;
                for msg in nav_messages {
                    let show_orphans = self.config.show_orphaned_commits;
                    match msg {
                        AppMessage::EnterSubmodule(name) => {
                            if let Some(rx) = enter_submodule(
                                &name,
                                repo_tab,
                                view_state,
                                &state.text_renderer,
                                scale,
                                &mut self.toast_manager,
                                show_orphans,
                                &self.proxy,
                            ) {
                                nav_rx = Some(rx);
                            }
                        }
                        AppMessage::ExitSubmodule => {
                            if let Some(rx) = exit_submodule(
                                repo_tab,
                                view_state,
                                &state.text_renderer,
                                scale,
                                show_orphans,
                                &self.proxy,
                            ) {
                                nav_rx = Some(rx);
                            }
                        }
                        AppMessage::ExitToDepth(depth) => {
                            if let Some(rx) = exit_to_depth(
                                depth,
                                repo_tab,
                                view_state,
                                &state.text_renderer,
                                scale,
                                show_orphans,
                                &self.proxy,
                            ) {
                                nav_rx = Some(rx);
                            }
                        }
                        _ => unreachable!(),
                    }
                }
            }
            if nav_rx.is_some() {
                view_state.repo_state_receiver = nav_rx;
            }
            // Mark status dirty after submodule navigation to refresh staging well
            self.status_dirty = true;
        }

        if normal_messages.is_empty() {
            return;
        }

        let scale = self
            .state
            .as_ref()
            .map(|s| s.scale_factor as f32)
            .unwrap_or(1.0);

        // Compute graph bounds for JumpToWorktreeBranch
        let graph_bounds = if let Some(ref state) = self.state {
            let extent = state.surface.extent();
            let screen_bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
            let tab_bar_height = if tab_count > 1 {
                TabBar::height(scale)
            } else {
                0.0
            };
            let (_tab_bar_bounds, main_bounds) = screen_bounds.take_top(tab_bar_height);
            let layout = ScreenLayout::compute_with_ratios_and_shortcut(
                main_bounds,
                4.0,
                scale,
                Some(self.sidebar_ratio),
                Some(self.graph_ratio),
                self.shortcut_bar_visible,
            );
            layout.graph
        } else {
            Rect::new(0.0, 0.0, 1920.0, 1080.0)
        };

        let ctx = MessageContext {
            graph_bounds,
            show_orphaned_commits: self.config.show_orphaned_commits,
        };

        let repo = &repo_tab.repo;

        // Any normal message likely changes state, so mark status dirty
        self.status_dirty = true;

        // Resolve staging_repo via direct field access to avoid borrow conflict
        // (repo_cache is immutably borrowed, worktrees is mutably borrowed in msg_view_state)
        let staging_repo: &GitRepo = view_state
            .worktree_state
            .selected_path
            .as_ref()
            .and_then(|p| view_state.worktree_state.repo_cache.get(p))
            .unwrap_or(repo);
        let mut needs_repo_refresh = false;
        for msg in normal_messages {
            let mut msg_view_state = MessageViewState {
                commit_graph_view: &mut view_state.commit_graph_view,
                staging_well: &mut view_state.staging_well,
                diff_view: &mut view_state.diff_view,
                commit_detail_view: &mut view_state.commit_detail_view,
                branch_sidebar: &mut view_state.branch_sidebar,
                header_bar: &mut view_state.header_bar,
                last_diff_commit: &mut view_state.last_diff_commit,
                fetch_receiver: &mut view_state.fetch_receiver,
                pull_receiver: &mut view_state.pull_receiver,
                push_receiver: &mut view_state.push_receiver,
                generic_op_receiver: &mut view_state.generic_op_receiver,
                right_panel_mode: &mut view_state.right_panel_mode,
                worktrees: &mut view_state.worktree_state.worktrees,
                proxy: self.proxy.clone(),
                needs_repo_refresh: false,
            };
            handle_app_message(
                msg,
                repo,
                staging_repo,
                &mut repo_tab.commits,
                &mut msg_view_state,
                &mut self.toast_manager,
                &ctx,
            );
            needs_repo_refresh |= msg_view_state.needs_repo_refresh;
        }
        if needs_repo_refresh {
            self.trigger_repo_state_refresh_for_tab(self.active_tab);
        }
    }

    fn close_tab(&mut self, index: usize) {
        if self.tabs.len() <= 1 {
            return; // Don't close the last tab
        }
        let name = self.tabs[index].0.name.clone();
        self.tabs.remove(index);
        self.active_tab = self.tab_bar.remove_tab(index);
        self.toast_manager
            .push(format!("Closed {}", name), ToastSeverity::Info);
        self.refresh_status();
    }

    fn switch_tab(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active_tab = index;
            self.tab_bar.set_active(index);
            self.refresh_status();
        }
    }

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
        let Some(state) = &mut self.state else { return };

        match event {
            WindowEvent::CloseRequested => {
                crash_log::mark_clean_exit();
                event_loop.exit();
            }

            WindowEvent::Resized(_) => {
                self.resize_debounce = Some(Instant::now());
                state.window.request_redraw();
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                state.scale_factor = scale_factor;
                // Always update render scale immediately — the SDF atlas still works
                // at different scales, just with slightly lower quality until the
                // new atlas is ready.
                state.text_renderer.set_render_scale(scale_factor);
                state.bold_text_renderer.set_render_scale(scale_factor);

                let atlas_build_scale = state.text_renderer.atlas_build_scale() as f64;
                let scale_ratio = if atlas_build_scale > 0.0 {
                    scale_factor / atlas_build_scale
                } else {
                    1.0
                };
                let needs_rebuild = scale_ratio >= TEXT_REBUILD_SCALE_UP_RATIO
                    || scale_ratio <= TEXT_REBUILD_SCALE_DOWN_RATIO;
                if needs_rebuild {
                    if std::env::var_os("WHISPER_TEXT_DIAG").is_some() {
                        eprintln!(
                            "text_diag scale_change: rebuilding atlas from {:.2} -> {:.2} (ratio {:.3})",
                            atlas_build_scale, scale_factor, scale_ratio
                        );
                    }
                    // Spawn atlas rebuild on background thread to avoid blocking
                    // the event loop (which causes Wayland disconnects).
                    let cba = state.ctx.command_buffer_allocator.clone();
                    let queue = state.ctx.queue.clone();
                    let mem = state.ctx.memory_allocator.clone();
                    let rp = state.surface.render_pass.clone();
                    let dev = state.ctx.device.clone();
                    let proxy = self.proxy.clone();
                    let (tx, rx) = std::sync::mpsc::channel();
                    self.text_rebuild_receiver = Some(rx);
                    std::thread::spawn(move || {
                        match build_text_renderers(
                            cba,
                            queue,
                            mem,
                            rp,
                            dev,
                            scale_factor,
                            scale_factor,
                        ) {
                            Ok(renderers) => {
                                let _ = tx.send(renderers);
                            }
                            Err(e) => {
                                eprintln!("Failed to rebuild text atlases: {e:?}");
                            }
                        }
                        let _ = proxy.send_event(());
                    });
                }
                for (repo_tab, view_state) in &mut self.tabs {
                    view_state
                        .commit_graph_view
                        .sync_metrics(&state.text_renderer);
                    view_state
                        .commit_graph_view
                        .update_layout(&repo_tab.commits);
                    view_state.branch_sidebar.sync_metrics(&state.text_renderer);
                    view_state.staging_well.set_scale(scale_factor as f32);
                }
                state.surface.needs_recreate = true;
                state.window.request_redraw();
            }

            WindowEvent::RedrawRequested => {
                self.last_frame_time = Instant::now();
                let frame_diag = std::env::var_os("WHISPER_FRAME_DIAG").is_some();
                let frame_t0 = Instant::now();

                // Poll background text renderer rebuild (HiDPI monitor switch)
                if let Some(ref rx) = self.text_rebuild_receiver
                    && let Ok((text, bold)) = rx.try_recv()
                {
                    state.text_renderer = text;
                    state.bold_text_renderer = bold;
                    self.text_rebuild_receiver = None;
                    // Re-sync all tab metrics with the new atlas
                    for (repo_tab, view_state) in &mut self.tabs {
                        view_state
                            .commit_graph_view
                            .sync_metrics(&state.text_renderer);
                        view_state
                            .commit_graph_view
                            .update_layout(&repo_tab.commits);
                        view_state.branch_sidebar.sync_metrics(&state.text_renderer);
                    }
                    state.window.request_redraw();
                }

                // Poll async diff stats FIRST — apply completed results before
                // watcher or remote ops can orphan the receiver with a new one
                self.poll_diff_stats();
                // Re-launch diff stats if the previous receiver was orphaned
                self.ensure_diff_stats();
                // Poll background status refresh
                self.poll_status();
                // Poll background repo state refresh
                let t = Instant::now();
                self.poll_repo_state();
                if frame_diag {
                    let d = t.elapsed();
                    if d.as_millis() > 0 {
                        eprintln!(
                            "[frame_diag] poll_repo_state: {:.1}ms",
                            d.as_secs_f64() * 1000.0
                        );
                    }
                }
                // Poll per-entity dirty check results (submodule/worktree)
                self.poll_dirty_checks();
                // Poll asynchronous watcher initialization (repo open / submodule navigation).
                self.poll_watcher_init();
                // Poll filesystem watcher for external changes
                self.poll_watcher();
                // Poll background remote operations
                self.poll_remote_ops();
                // Poll AI commit message generation
                self.poll_ai_commit();
                // Finalize diagnostic reload if both async results have arrived
                if self.diagnostic_before.is_some() {
                    self.finalize_diagnostic_reload();
                }
                // Process any pending messages
                let t = Instant::now();
                self.process_messages();
                if frame_diag {
                    let d = t.elapsed();
                    if d.as_millis() > 0 {
                        eprintln!(
                            "[frame_diag] process_messages: {:.1}ms",
                            d.as_secs_f64() * 1000.0
                        );
                    }
                }
                // Check if staging well requested an immediate status refresh (e.g., worktree switch)
                if let Some((_rt, vs)) = self.tabs.get_mut(self.active_tab)
                    && vs.staging_well.status_refresh_needed
                {
                    self.status_dirty = true;
                    vs.staging_well.status_refresh_needed = false;
                }
                self.poll_status_refresh_timer();
                self.poll_ref_reconciliation();
                self.poll_ci_refresh();

                self.poll_repo_dialog();
                self.poll_clone_dialog();
                self.poll_clone_receiver();
                self.poll_open_receiver(frame_diag);

                // Debounce swapchain recreation during rapid resizes (e.g. KDE
                // animated window geometry changes).  Render at the old swapchain
                // size until resizes settle, then recreate once.
                if let Some(last_resize) = self.resize_debounce
                    && last_resize.elapsed() >= Duration::from_millis(100)
                {
                    let state = self.state.as_mut().unwrap();
                    state.surface.needs_recreate = true;
                    self.resize_debounce = None;
                }

                let t = Instant::now();
                match draw_frame(self) {
                    Ok(()) => {
                        self.consecutive_draw_errors = 0;
                    }
                    Err(e) => {
                        self.consecutive_draw_errors += 1;
                        crash_log::breadcrumb(format!("draw_frame error: {e:?}"));
                        eprintln!("Draw error (#{}):{e:?}", self.consecutive_draw_errors);
                    }
                }
                if frame_diag {
                    let d = t.elapsed();
                    if d.as_millis() > 0 {
                        eprintln!("[frame_diag] draw_frame: {:.1}ms", d.as_secs_f64() * 1000.0);
                    }
                }

                self.last_completed_frame = Instant::now();

                if frame_diag {
                    let total = frame_t0.elapsed();
                    if total.as_millis() > 2 {
                        eprintln!(
                            "[frame_diag] === total frame: {:.1}ms ===",
                            total.as_secs_f64() * 1000.0
                        );
                    }
                }

                // Screenshot mode
                let screenshot_path = self.cli_args.screenshot.clone();
                if let Some(path) = screenshot_path {
                    let has_state = self.cli_args.screenshot_state.is_some();
                    let capture_frame = if has_state { 4 } else { 3 };
                    let frame = self.state.as_ref().unwrap().frame_count;

                    // Apply injected state one frame before capture
                    if frame == 3 && has_state {
                        apply_screenshot_state(self);
                    }

                    if frame == capture_frame {
                        let screenshot_size = self.cli_args.screenshot_size;
                        let result = if let Some((width, height)) = screenshot_size {
                            capture_screenshot_offscreen(self, width, height)
                        } else {
                            capture_screenshot(self)
                        };
                        match result {
                            Ok(img) => {
                                if let Err(e) = img.save(&path) {
                                    eprintln!("Failed to save screenshot: {e}");
                                } else {
                                    println!("Screenshot saved to: {}", path.display());
                                }
                            }
                            Err(e) => eprintln!("Failed to capture screenshot: {e:?}"),
                        }
                        event_loop.exit();
                        return;
                    }
                    // Screenshot mode needs continuous redraws to reach capture frame
                    if let Some(state) = &self.state {
                        state.window.request_redraw();
                    }
                }
            }

            // Handle input events
            ref win_event => {
                // Convert winit event to our InputEvent (brief mutable borrow)
                let input_event = state.input_state.handle_window_event(win_event);
                if let Some(input_event) = input_event {
                    self.handle_input_event(event_loop, &input_event);
                    // Any user input should trigger a visual update
                    if let Some(state) = &self.state {
                        state.window.request_redraw();
                    }
                }
            }
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

