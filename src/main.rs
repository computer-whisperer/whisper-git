//! Main application entry point and event loop.
//!
//! Owns the App struct, winit event loop, Vulkan draw pipeline, and three-layer rendering architecture
//! (base → chrome → overlay). Handles async git operations via mpsc channels and thread spawning.

mod ai;
mod async_polling;
mod ci;
mod config;
mod crash_log;
mod git;
mod github;
mod gitlab;
mod input;
mod messages;
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
use crate::git::{CommitInfo, GitRepo, RemoteOpResult, SubmoduleInfo, WorktreeInfo};
use crate::input::{InputEvent, InputState, Key};
use crate::messages::{
    AppMessage, MessageContext, MessageViewState, RepoStateSnapshot, RightPanelMode,
    compute_reload_deltas, handle_app_message,
};
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
    pub(crate) fetch_receiver: Option<(Receiver<RemoteOpResult>, Instant, String)>,
    pub(crate) pull_receiver: Option<(Receiver<RemoteOpResult>, Instant, String)>,
    pub(crate) push_receiver: Option<(Receiver<RemoteOpResult>, Instant, String)>,
    /// Generic async receiver for submodule/worktree ops (label for toast)
    pub(crate) generic_op_receiver: Option<(Receiver<RemoteOpResult>, String, Instant)>,
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
        eprintln!(
            "  In-flight: repo_state={} status={} diff_stats={} open={} clone={} text_rebuild={}",
            app.repo_state_receiver.is_some(),
            app.status_receiver.is_some(),
            app.diff_stats_receiver.is_some(),
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
    /// Receiver for async diff stats computation
    pub(crate) diff_stats_receiver: Option<Receiver<Vec<(Oid, usize, usize)>>>,
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
    /// Receiver for async status refresh (background thread)
    pub(crate) status_receiver: Option<Receiver<StatusResult>>,
    /// Receiver for async repo state refresh (background thread)
    pub(crate) repo_state_receiver: Option<Receiver<RepoStateResult>>,
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
                            repo,
                            commits: Vec::new(),
                            name,
                        },
                        TabViewState::new(),
                    ));
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
            diff_stats_receiver: None,
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
            status_receiver: None,
            repo_state_receiver: None,
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
            self.repo_state_receiver = Some(init_tab_view(
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

    fn refresh_status(&mut self) {
        if let Some((repo_tab, view_state)) = self.tabs.get(self.active_tab) {
            let repo = &repo_tab.repo;
            let repo_context_path = repo
                .workdir()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| repo.git_dir().to_path_buf());
            let is_bare = repo.is_effectively_bare();

            // Determine staging repo context path (selected worktree or same as main)
            let staging_context_path = view_state
                .worktree_state
                .staging_repo()
                .map(|r| {
                    r.workdir()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| r.git_dir().to_path_buf())
                })
                .or_else(|| Some(repo_context_path.clone()));

            self.status_receiver = Some(spawn_status_refresh(
                repo_context_path,
                staging_context_path,
                is_bare,
                self.proxy.clone(),
            ));
        }
    }

    fn poll_status(&mut self) {
        let rx = match self.status_receiver {
            Some(ref rx) => rx,
            None => return,
        };
        match rx.try_recv() {
            Ok(result) => {
                self.status_receiver = None;
                if let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) {
                    apply_status_result(result, repo_tab, view_state);
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.status_receiver = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }

    /// Spawn an async repo state refresh for the active tab.
    fn trigger_repo_state_refresh(&mut self) {
        if let Some((repo_tab, view_state)) = self.tabs.get(self.active_tab) {
            let repo_context_path = repo_tab
                .repo
                .workdir()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| repo_tab.repo.git_dir().to_path_buf());
            let staging_context_path = view_state
                .worktree_state
                .staging_repo()
                .map(|r| {
                    r.workdir()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| r.git_dir().to_path_buf())
                })
                .or_else(|| Some(repo_context_path.clone()));

            self.repo_state_receiver = Some(spawn_repo_state_refresh(
                repo_context_path,
                staging_context_path,
                self.config.show_orphaned_commits,
                self.proxy.clone(),
            ));
        }
    }

    fn poll_repo_state(&mut self) {
        let rx = match self.repo_state_receiver {
            Some(ref rx) => rx,
            None => return,
        };
        match rx.try_recv() {
            Ok(result) => {
                self.repo_state_receiver = None;
                crash_log::breadcrumb(format!(
                    "apply_repo_state: {} commits, {} worktrees",
                    result.commits.len(),
                    result.worktrees.len()
                ));
                if let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) {
                    let rx = apply_repo_state_result(
                        result,
                        repo_tab,
                        view_state,
                        &mut self.toast_manager,
                        &self.proxy,
                    );
                    if rx.is_some() {
                        self.diff_stats_receiver = rx;
                    }
                    // Update watcher paths in case worktree structure changed
                    let common_dir = repo_tab.repo.common_dir().to_path_buf();
                    if let Some(ref mut w) = view_state.watcher {
                        w.update_worktree_watches(
                            &view_state.worktree_state.worktrees,
                            &common_dir,
                        );
                    }
                    // Kick off per-entity dirty checks for submodules and worktrees
                    let repo_workdir = repo_tab.repo.workdir().map(|p| p.to_path_buf());
                    self.dirty_checks_in_flight += spawn_dirty_checks(
                        &view_state.staging_well.submodules,
                        &view_state.worktree_state.worktrees,
                        repo_workdir,
                        &self.dirty_check_tx,
                        &self.proxy,
                    );
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.repo_state_receiver = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }

    /// Poll per-entity dirty check results and apply them individually.
    fn poll_dirty_checks(&mut self) {
        if self.dirty_checks_in_flight == 0 {
            return;
        }
        loop {
            match self.dirty_check_rx.try_recv() {
                Ok(result) => {
                    self.dirty_checks_in_flight = self.dirty_checks_in_flight.saturating_sub(1);
                    if let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) {
                        apply_dirty_check_result(result, repo_tab, view_state);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // Channel closed — should never happen since we hold the sender
                    self.dirty_checks_in_flight = 0;
                    break;
                }
            }
        }
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
        if self.repo_state_receiver.is_some() || self.status_receiver.is_some() {
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
                self.repo_state_receiver = nav_rx;
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
            self.trigger_repo_state_refresh();
        }
    }

    /// Re-launch async diff stats for any commits still missing stats.
    /// Runs every frame so orphaned receivers are quickly replaced.
    fn ensure_diff_stats(&mut self) {
        if self.diff_stats_receiver.is_some() {
            return; // computation already in progress
        }
        let Some((repo_tab, _view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let repo = &repo_tab.repo;
        let needs_stats: Vec<Oid> = repo_tab
            .commits
            .iter()
            .filter(|c| !c.is_synthetic && c.insertions == 0 && c.deletions == 0)
            .map(|c| c.id)
            .collect();
        if !needs_stats.is_empty() {
            self.diff_stats_receiver =
                Some(repo.compute_diff_stats_async(needs_stats, self.proxy.clone()));
        }
    }

    fn poll_watcher_init(&mut self) {
        let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let Some(ref rx) = view_state.watcher_init_receiver else {
            return;
        };

        match rx.try_recv() {
            Ok(Ok((watcher, watcher_rx))) => {
                view_state.watcher_init_receiver = None;
                view_state.watcher = Some(watcher);
                view_state.watcher_rx = Some(watcher_rx);
            }
            Ok(Err(err)) => {
                view_state.watcher_init_receiver = None;
                view_state.watcher = None;
                view_state.watcher_rx = None;
                self.toast_manager.push(
                    format!("Filesystem watcher failed: {}", err),
                    ToastSeverity::Error,
                );
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                view_state.watcher_init_receiver = None;
                view_state.watcher = None;
                view_state.watcher_rx = None;
                self.toast_manager.push(
                    "Filesystem watcher failed: background thread terminated".to_string(),
                    ToastSeverity::Error,
                );
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }

    fn poll_watcher(&mut self) {
        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let Some(ref rx) = view_state.watcher_rx else {
            return;
        };

        // Drain all pending signals, track the highest-priority kind
        let mut max_kind: Option<FsChangeKind> = None;
        while let Ok(kind) = rx.try_recv() {
            max_kind = Some(match max_kind {
                Some(prev) => {
                    if kind.priority() > prev.priority() {
                        kind
                    } else {
                        prev
                    }
                }
                None => kind,
            });
        }

        match max_kind {
            Some(FsChangeKind::WorkingTree) => {
                // Lightweight: just mark status dirty, no commit graph rebuild
                self.status_dirty = true;
                // Re-check worktree dirty state (file may have changed in a worktree)
                self.dirty_checks_in_flight += spawn_dirty_checks(
                    &[], // skip submodules for working tree changes
                    &view_state.worktree_state.worktrees,
                    repo_tab.repo.workdir().map(|p| p.to_path_buf()),
                    &self.dirty_check_tx,
                    &self.proxy,
                );
            }
            Some(FsChangeKind::GitMetadata) => {
                self.status_dirty = true;
                // Force-reopen repo handles to bypass libgit2 refdb cache
                let _ = repo_tab.repo.reopen();
                for wt_repo in view_state.worktree_state.repo_cache.values_mut() {
                    let _ = wt_repo.reopen();
                }
                self.trigger_repo_state_refresh();
            }
            Some(FsChangeKind::WorktreeStructure) => {
                self.status_dirty = true;
                // Force-reopen repo handles to bypass libgit2 refdb cache
                let _ = repo_tab.repo.reopen();
                for wt_repo in view_state.worktree_state.repo_cache.values_mut() {
                    let _ = wt_repo.reopen();
                }
                self.trigger_repo_state_refresh();
                // Note: watcher path updates will happen when poll_repo_state applies results
            }
            None => {}
        }
    }

    fn poll_diff_stats(&mut self) {
        let rx = match self.diff_stats_receiver {
            Some(ref rx) => rx,
            None => return,
        };
        match rx.try_recv() {
            Ok(stats) => {
                self.diff_stats_receiver = None;
                if let Some((repo_tab, _view_state)) = self.tabs.get_mut(self.active_tab) {
                    for (oid, ins, del) in stats {
                        if let Some(commit) = repo_tab.commits.iter_mut().find(|c| c.id == oid) {
                            commit.insertions = ins;
                            commit.deletions = del;
                        }
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.diff_stats_receiver = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }

    fn poll_remote_ops(&mut self) {
        use std::sync::mpsc::TryRecvError;

        let ci_config = self.config.clone();
        let proxy = self.proxy.clone();
        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let now = Instant::now();
        const TIMEOUT_SECS: u64 = 60;

        let mut needs_repo_refresh = false;

        // Helper: handle the result of polling a fetch/pull/push op.
        // On success, shows a toast and sets needs_repo_refresh flag.
        macro_rules! handle_poll {
            ($op_name:expr, $past_tense:expr, $receiver:expr, $header_flag:expr, $timeout_idx:expr) => {
                match poll_remote_op(
                    $receiver,
                    $header_flag,
                    &mut view_state.showed_timeout_toast[$timeout_idx],
                    $op_name,
                    now,
                    TIMEOUT_SECS,
                ) {
                    AsyncOpPoll::Success(remote) => {
                        self.toast_manager.push(
                            format!("{} {}", $past_tense, remote),
                            ToastSeverity::Success,
                        );
                        needs_repo_refresh = true;
                        // Refresh CI status after remote ops
                        trigger_ci_fetch(&ci_config, repo_tab, view_state, &proxy);
                    }
                    AsyncOpPoll::Failed(summary, raw_stderr) => {
                        self.error_dialog.show(
                            &format!("{} Failed", $op_name),
                            &summary,
                            &raw_stderr,
                        );
                        self.interrupted_modal = self.active_modal.take();
                        self.active_modal = Some(ActiveModal::Error);
                        // Refresh even on failure: a failed pull still fetches refs,
                        // a failed merge may leave the repo in a new state, etc.
                        needs_repo_refresh = true;
                    }
                    AsyncOpPoll::Disconnected => {
                        self.error_dialog.show(
                            &format!("{} Failed", $op_name),
                            &format!(
                                "{} failed: the background thread terminated unexpectedly.",
                                $op_name
                            ),
                            "",
                        );
                        self.interrupted_modal = self.active_modal.take();
                        self.active_modal = Some(ActiveModal::Error);
                    }
                    AsyncOpPoll::Timeout => {
                        self.toast_manager.push(
                            format!("{} still running...", $op_name),
                            ToastSeverity::Info,
                        );
                    }
                    AsyncOpPoll::Pending => {}
                }
            };
        }

        handle_poll!(
            "Fetch",
            "Fetched from",
            &mut view_state.fetch_receiver,
            &mut view_state.header_bar.fetching,
            0
        );
        handle_poll!(
            "Pull",
            "Pulled from",
            &mut view_state.pull_receiver,
            &mut view_state.header_bar.pulling,
            1
        );
        let was_pushing = view_state.header_bar.pushing;
        handle_poll!(
            "Push",
            "Pushed to",
            &mut view_state.push_receiver,
            &mut view_state.header_bar.pushing,
            2
        );
        if was_pushing && !view_state.header_bar.pushing {
            view_state.last_push_time = Some(Instant::now());
        }

        // Poll CI status receivers (one per provider)
        view_state.ci_receivers.retain(|rx| {
            match rx.try_recv() {
                Ok(result) => {
                    // Replace any existing result for the same provider
                    view_state
                        .ci_results
                        .retain(|r| r.provider != result.provider);
                    view_state.ci_results.push(result);
                    view_state.ci_results.sort_by_key(|r| r.provider.sort_key());
                    false // remove completed receiver
                }
                _ => true, // keep pending receivers
            }
        });
        // Update per-commit states from merged provider results
        if !view_state.ci_results.is_empty() {
            let fetch = ci::CiFetchResult {
                providers: view_state.ci_results.clone(),
            };
            view_state.commit_graph_view.ci_commit_rollups = fetch.per_commit_provider_rollups();
        }

        // Poll generic async ops (submodule/worktree operations)
        // This has unique post-success behavior (squash merge toast, worktree/stash refresh)
        // so it's handled separately rather than through the common helper.
        if let Some((ref rx, ref label, started)) = view_state.generic_op_receiver {
            let label = label.clone();
            match rx.try_recv() {
                Ok(result) => {
                    view_state.generic_op_receiver = None;
                    view_state.showed_timeout_toast[3] = false;
                    if result.success {
                        self.toast_manager
                            .push(format!("{} complete", label), ToastSeverity::Success);
                        // Squash merge doesn't auto-commit: show info toast
                        if label.starts_with("Squash merge") {
                            self.toast_manager.push(
                                "Squash merge staged. Review and commit when ready.".to_string(),
                                ToastSeverity::Info,
                            );
                        }
                        needs_repo_refresh = true;
                    } else {
                        let (msg, _) = git::classify_git_error(&label, &result.error);
                        self.error_dialog
                            .show(&format!("{} Failed", label), &msg, &result.error);
                        self.interrupted_modal = self.active_modal.take();
                        self.active_modal = Some(ActiveModal::Error);
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    view_state.generic_op_receiver = None;
                    view_state.showed_timeout_toast[3] = false;
                    self.error_dialog.show(
                        &format!("{} Failed", label),
                        &format!(
                            "{} failed: the background thread terminated unexpectedly.",
                            label
                        ),
                        "",
                    );
                    self.interrupted_modal = self.active_modal.take();
                    self.active_modal = Some(ActiveModal::Error);
                }
                Err(TryRecvError::Empty) => {
                    if now.duration_since(started).as_secs() >= TIMEOUT_SECS
                        && !view_state.showed_timeout_toast[3]
                    {
                        view_state.showed_timeout_toast[3] = true;
                        self.toast_manager
                            .push(format!("{} still running...", label), ToastSeverity::Info);
                    }
                }
            }
        }

        if needs_repo_refresh {
            self.trigger_repo_state_refresh();
            self.status_dirty = true;
        }
    }

    fn poll_ai_commit(&mut self) {
        let (rx, started) = match self.ai_commit_receiver {
            Some(ref inner) => inner,
            None => return,
        };
        let started = *started;

        match rx.try_recv() {
            Ok(Ok(response)) => {
                self.ai_commit_receiver = None;
                if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                    view_state.staging_well.ai_generating = false;
                    view_state
                        .staging_well
                        .subject_input
                        .set_text(&response.subject);
                    view_state.staging_well.body_area.set_text(&response.body);
                }
                self.toast_manager.push(
                    "Commit message generated".to_string(),
                    ToastSeverity::Success,
                );
            }
            Ok(Err(err)) => {
                self.ai_commit_receiver = None;
                if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                    view_state.staging_well.ai_generating = false;
                }
                self.toast_manager.push(
                    format!("AI generation failed: {}", err),
                    ToastSeverity::Error,
                );
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.ai_commit_receiver = None;
                if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                    view_state.staging_well.ai_generating = false;
                }
                self.toast_manager.push(
                    "AI generation failed: thread terminated".to_string(),
                    ToastSeverity::Error,
                );
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // Check for timeout (30s)
                if started.elapsed().as_secs() >= 30 {
                    self.ai_commit_receiver = None;
                    if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                        view_state.staging_well.ai_generating = false;
                    }
                    self.toast_manager
                        .push("AI generation timed out".to_string(), ToastSeverity::Error);
                }
            }
        }
    }

    /// Open a new repo and add it as a tab
    fn start_clone(&mut self, url: String, dest: PathBuf, bare: bool) {
        if self.clone_receiver.is_some() {
            self.toast_manager.push(
                "A clone is already in progress".to_string(),
                ToastSeverity::Info,
            );
            return;
        }
        let dest_display = dest.display().to_string();
        let label = if bare { "Bare cloning" } else { "Cloning" };
        self.toast_manager.push(
            format!("{label} into {dest_display}..."),
            ToastSeverity::Info,
        );
        let (tx, rx) = std::sync::mpsc::channel::<Result<PathBuf, String>>();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let mut args = vec!["clone"];
            if bare {
                args.push("--bare");
            }
            args.push(&url);
            let dest_str = dest.to_string_lossy().to_string();
            args.push(&dest_str);
            let result = std::process::Command::new("git")
                .args(&args)
                .env("GIT_TERMINAL_PROMPT", "0")
                .output();
            let clone_result = match result {
                Ok(output) if output.status.success() => Ok(dest),
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    Err(stderr)
                }
                Err(e) => Err(format!("Failed to run git clone: {e}")),
            };
            let _ = tx.send(clone_result);
            let _ = proxy.send_event(());
        });
        self.clone_receiver = Some((rx, Instant::now()));
    }

    /// Spawn a background thread to open a repo, avoiding main-thread stalls
    /// that can cause Wayland disconnects.
    fn open_repo_tab(&mut self, path: PathBuf) {
        crash_log::breadcrumb(format!("open_repo_tab: {}", path.display()));
        if self.open_receiver.is_some() {
            self.toast_manager.push(
                "A repo is already being opened".to_string(),
                ToastSeverity::Info,
            );
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = GitRepo::open(&path)
                .map(|repo| {
                    let name = repo.repo_name();
                    (repo, name)
                })
                .map_err(|e| format!("{e}"));
            let _ = tx.send(result);
            let _ = proxy.send_event(());
        });
        self.open_receiver = Some(rx);
    }

    /// Finish creating a tab after the background GitRepo::open completes.
    fn finish_open_repo_tab(&mut self, repo: GitRepo, name: String) {
        self.tab_bar.add_tab(name.clone());
        let mut view_state = TabViewState::new();

        let mut repo_tab = RepoTab {
            repo,
            commits: Vec::new(),
            name,
        };

        if let Some(ref render_state) = self.state {
            view_state.commit_graph_view.row_scale = self.settings_dialog.row_scale;
            view_state.commit_graph_view.abbreviate_worktree_names =
                self.settings_dialog.abbreviate_worktree_names;
            view_state.commit_graph_view.time_spacing_strength =
                self.settings_dialog.time_spacing_strength;
            view_state.commit_graph_view.fast_scroll = self.config.fast_scroll;
            view_state.commit_graph_view.ratchet_scroll = self.config.ratchet_scroll;
            self.repo_state_receiver = Some(init_tab_view(
                &mut repo_tab,
                &mut view_state,
                &render_state.text_renderer,
                render_state.scale_factor as f32,
                self.config.show_orphaned_commits,
                &self.proxy,
            ));
        }

        trigger_ci_fetch(&self.config, &repo_tab, &mut view_state, &self.proxy);
        self.tabs.push((repo_tab, view_state));
        let new_idx = self.tabs.len() - 1;
        self.active_tab = new_idx;
        self.tab_bar.set_active(new_idx);
        self.toast_manager.push(
            format!("Opened {}", self.tabs[new_idx].0.name),
            ToastSeverity::Success,
        );
    }

    /// Apply settings dialog values to config and views.
    fn apply_settings_changes(&mut self) {
        let row_scale = self.settings_dialog.row_scale;
        let abbreviate_wt = self.settings_dialog.abbreviate_worktree_names;
        let time_strength = self.settings_dialog.time_spacing_strength;
        let fast_scroll = self.settings_dialog.scroll_speed >= 1.5;
        let ratchet_scroll = self.settings_dialog.ratchet_scroll;
        let orphans_changed =
            self.config.show_orphaned_commits != self.settings_dialog.show_orphaned_commits;
        if let Some(ref state) = self.state {
            for (repo_tab, view_state) in &mut self.tabs {
                view_state.commit_graph_view.row_scale = row_scale;
                view_state.commit_graph_view.abbreviate_worktree_names = abbreviate_wt;
                view_state.commit_graph_view.time_spacing_strength = time_strength;
                view_state.commit_graph_view.fast_scroll = fast_scroll;
                view_state.commit_graph_view.ratchet_scroll = ratchet_scroll;
                view_state
                    .commit_graph_view
                    .sync_metrics(&state.text_renderer);
                view_state
                    .commit_graph_view
                    .compute_row_offsets(&repo_tab.commits);
            }
        }
        self.config.avatars_enabled = self.settings_dialog.show_avatars;
        self.config.fast_scroll = fast_scroll;
        self.config.row_scale = self.settings_dialog.row_scale;
        self.config.abbreviate_worktree_names = self.settings_dialog.abbreviate_worktree_names;
        self.config.time_spacing_strength = self.settings_dialog.time_spacing_strength;
        self.config.show_orphaned_commits = self.settings_dialog.show_orphaned_commits;
        self.config.ratchet_scroll = self.settings_dialog.ratchet_scroll;
        if let Err(e) = self.config.save() {
            self.toast_manager.push(e, ToastSeverity::Error);
        }
        if orphans_changed {
            self.trigger_repo_state_refresh();
        }
    }

    /// Open the token management dialog with current keychain state.
    fn open_token_dialog(&mut self) {
        let github_has_token = token_store::get_github_token().is_some()
            || self
                .config
                .github_token
                .as_ref()
                .is_some_and(|t| !t.is_empty());

        // Collect known GitLab hosts from keychain + config
        let mut gitlab_hosts: Vec<(String, bool)> = Vec::new();
        // From config (may still have hosts listed even if tokens migrated)
        for host in self.config.gitlab_tokens.keys() {
            let has_token = token_store::get_gitlab_token(host).is_some()
                || self
                    .config
                    .gitlab_tokens
                    .get(host)
                    .is_some_and(|t| !t.is_empty());
            gitlab_hosts.push((host.clone(), has_token));
        }
        self.token_dialog.show(github_has_token, gitlab_hosts);
        self.active_modal = Some(ActiveModal::TokenManager);
    }

    /// Handle an action from the token dialog.
    fn handle_token_action(&mut self, action: TokenDialogAction) {
        match action {
            TokenDialogAction::Close => {}
            TokenDialogAction::SetGitHubToken(token) => {
                if token.is_empty() {
                    // Clear
                    token_store::delete_github_token();
                    self.config.github_token = None;
                    let _ = self.config.save();
                    self.toast_manager
                        .push("GitHub token removed", ToastSeverity::Success);
                } else if token_store::set_github_token(&token) {
                    // Stored in keychain — clear plaintext
                    self.config.github_token = None;
                    let _ = self.config.save();
                    self.toast_manager
                        .push("GitHub token saved to keychain", ToastSeverity::Success);
                } else {
                    // Keychain unavailable — fall back to plaintext
                    self.config.github_token = Some(token);
                    let _ = self.config.save();
                    self.toast_manager.push(
                        "GitHub token saved to config (keychain unavailable)",
                        ToastSeverity::Success,
                    );
                }
            }
            TokenDialogAction::SetGitLabToken { host, token } => {
                if token_store::set_gitlab_token(&host, &token) {
                    // Keep host in config as registry (empty value = stored in keychain)
                    self.config
                        .gitlab_tokens
                        .insert(host.clone(), String::new());
                    let _ = self.config.save();
                    self.toast_manager.push(
                        format!("GitLab token for {host} saved to keychain"),
                        ToastSeverity::Success,
                    );
                } else {
                    // Fallback to plaintext
                    self.config.gitlab_tokens.insert(host.clone(), token);
                    let _ = self.config.save();
                    self.toast_manager.push(
                        format!("GitLab token for {host} saved to config (keychain unavailable)"),
                        ToastSeverity::Success,
                    );
                }
            }
            TokenDialogAction::RemoveGitLabToken(host) => {
                token_store::delete_gitlab_token(&host);
                self.config.gitlab_tokens.remove(&host);
                let _ = self.config.save();
                self.toast_manager.push(
                    format!("GitLab token for {host} removed"),
                    ToastSeverity::Success,
                );
            }
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

    /// Handle events for modal dialogs (confirm, branch name, remote, settings, repo, toast, context menu).
    /// Returns true if the event was consumed by a modal.
    /// Set the active modal, hiding the previous one.
    fn set_active_modal(&mut self, modal: ActiveModal) {
        if let Some(prev) = self.active_modal.take() {
            self.set_modal_visible(prev, false);
        }
        self.set_modal_visible(modal, true);
        self.active_modal = Some(modal);
    }

    /// Close the active modal.
    fn close_active_modal(&mut self) {
        if let Some(modal) = self.active_modal.take() {
            self.set_modal_visible(modal, false);
        }
    }

    /// Open an interrupt modal (Error/Confirm) that will restore the previous modal on close.
    fn open_interrupt_modal(&mut self, modal: ActiveModal) {
        self.interrupted_modal = self.active_modal.take();
        if let Some(prev) = self.interrupted_modal {
            self.set_modal_visible(prev, false);
        }
        self.set_modal_visible(modal, true);
        self.active_modal = Some(modal);
    }

    /// Close an interrupt modal and restore whatever was active before.
    fn close_interrupt_modal(&mut self) {
        if let Some(modal) = self.active_modal.take() {
            self.set_modal_visible(modal, false);
        }
        if let Some(prev) = self.interrupted_modal.take() {
            self.set_modal_visible(prev, true);
            self.active_modal = Some(prev);
        }
    }

    /// Set the visible flag on a dialog by modal variant.
    fn set_modal_visible(&mut self, modal: ActiveModal, visible: bool) {
        match modal {
            ActiveModal::Settings => {
                if visible {
                    // show() is idempotent — just sets visible=true
                    self.settings_dialog.show();
                } else {
                    self.settings_dialog.hide();
                }
            }
            ActiveModal::TokenManager => {
                if !visible {
                    self.token_dialog.hide();
                }
                // show() requires params — handled by open_token_dialog()
            }
            ActiveModal::Confirm => {
                if !visible {
                    self.confirm_dialog.hide();
                }
            }
            ActiveModal::Error => {
                if !visible {
                    self.error_dialog.hide();
                }
            }
            ActiveModal::BranchName => {
                if !visible {
                    self.branch_name_dialog.hide();
                }
            }
            ActiveModal::Remote => {
                if !visible {
                    self.remote_dialog.hide();
                }
            }
            ActiveModal::Pull => {
                if !visible {
                    self.pull_dialog.hide();
                }
            }
            ActiveModal::Push => {
                if !visible {
                    self.push_dialog.hide();
                }
            }
            ActiveModal::Merge => {
                if !visible {
                    self.merge_dialog.hide();
                }
            }
            ActiveModal::Rebase => {
                if !visible {
                    self.rebase_dialog.hide();
                }
            }
            ActiveModal::RepoDialog => {
                if !visible {
                    self.repo_dialog.hide();
                }
            }
            ActiveModal::CloneDialog => {
                if !visible {
                    self.clone_dialog.hide();
                }
            }
        }
    }

    fn handle_modal_events(&mut self, input_event: &InputEvent, screen_bounds: Rect) -> bool {
        let Some(modal) = self.active_modal else {
            // No modal active — check non-modal overlays
            return self.handle_overlay_events(input_event, screen_bounds);
        };

        match modal {
            ActiveModal::Confirm => {
                self.confirm_dialog.handle_event(input_event, screen_bounds);
                if let Some(action) = self.confirm_dialog.take_action() {
                    match action {
                        ConfirmDialogAction::Confirm => {
                            if let Some(msg) = self.pending_confirm_action.take()
                                && let Some((_, view_state)) = self.tabs.get_mut(self.active_tab)
                            {
                                view_state.pending_messages.push(msg);
                            }
                        }
                        ConfirmDialogAction::Cancel => {
                            self.pending_confirm_action = None;
                        }
                    }
                    self.close_interrupt_modal();
                }
            }

            ActiveModal::Error => {
                self.error_dialog.handle_event(input_event, screen_bounds);
                if !self.error_dialog.is_visible() {
                    self.close_interrupt_modal();
                }
            }

            ActiveModal::BranchName => {
                let is_tag = self.branch_name_dialog.title().contains("Tag");
                self.branch_name_dialog
                    .handle_event(input_event, screen_bounds);
                if let Some(action) = self.branch_name_dialog.take_action() {
                    match action {
                        BranchNameDialogAction::Create(name, oid) => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                if is_tag {
                                    view_state
                                        .pending_messages
                                        .push(AppMessage::CreateTag(name, oid));
                                } else {
                                    view_state
                                        .pending_messages
                                        .push(AppMessage::CreateBranch(name, oid));
                                }
                            }
                        }
                        BranchNameDialogAction::CreateWorktree(
                            name,
                            source,
                            init_submodules,
                            checkout_lfs,
                        ) => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                view_state.pending_messages.push(AppMessage::CreateWorktree(
                                    name,
                                    source,
                                    init_submodules,
                                    checkout_lfs,
                                ));
                            }
                        }
                        BranchNameDialogAction::Rename(new_name, old_name) => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::RenameBranch(old_name, new_name));
                            }
                        }
                        BranchNameDialogAction::Cancel => {}
                    }
                    self.close_active_modal();
                }
            }

            ActiveModal::Remote => {
                self.remote_dialog.handle_event(input_event, screen_bounds);
                if let Some(action) = self.remote_dialog.take_action() {
                    match action {
                        RemoteDialogAction::AddRemote(name, url) => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::AddRemote(name, url));
                            }
                        }
                        RemoteDialogAction::EditUrl(name, url) => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::SetRemoteUrl(name, url));
                            }
                        }
                        RemoteDialogAction::Rename(old_name, new_name) => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::RenameRemote(old_name, new_name));
                            }
                        }
                        RemoteDialogAction::Cancel => {}
                    }
                    self.close_active_modal();
                }
            }

            ActiveModal::Pull => {
                self.pull_dialog.handle_event(input_event, screen_bounds);
                if let Some(action) = self.pull_dialog.take_action() {
                    match action {
                        PullDialogAction::Confirm {
                            remote,
                            branch,
                            rebase,
                        } => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::PullBranchFrom {
                                        remote,
                                        branch,
                                        rebase,
                                    });
                            }
                        }
                        PullDialogAction::Cancel => {}
                    }
                    self.close_active_modal();
                }
            }

            ActiveModal::Push => {
                self.push_dialog.handle_event(input_event, screen_bounds);
                if let Some(action) = self.push_dialog.take_action() {
                    match action {
                        PushDialogAction::Confirm {
                            local_branch,
                            remote,
                            remote_branch,
                            force,
                        } => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                view_state.pending_messages.push(AppMessage::PushBranchTo {
                                    local_branch,
                                    remote,
                                    remote_branch,
                                    force,
                                });
                            }
                        }
                        PushDialogAction::Cancel => {}
                    }
                    self.close_active_modal();
                }
            }

            ActiveModal::Merge => {
                self.merge_dialog.handle_event(input_event, screen_bounds);
                if let Some(action) = self.merge_dialog.take_action() {
                    match action {
                        MergeDialogAction::Confirm(branch, strategy, message, target_dir) => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                let msg = match strategy {
                                    MergeStrategy::Default => {
                                        AppMessage::MergeBranch(branch, target_dir)
                                    }
                                    MergeStrategy::NoFastForward => {
                                        let commit_msg = message.unwrap_or_else(|| {
                                            format!("Merge branch '{}'", branch)
                                        });
                                        AppMessage::MergeNoFf(branch, commit_msg, target_dir)
                                    }
                                    MergeStrategy::FastForwardOnly => {
                                        AppMessage::MergeFfOnly(branch, target_dir)
                                    }
                                    MergeStrategy::Squash => {
                                        AppMessage::MergeSquash(branch, target_dir)
                                    }
                                };
                                view_state.pending_messages.push(msg);
                            }
                        }
                        MergeDialogAction::Cancel => {}
                    }
                    self.close_active_modal();
                }
            }

            ActiveModal::Rebase => {
                self.rebase_dialog.handle_event(input_event, screen_bounds);
                if let Some(action) = self.rebase_dialog.take_action() {
                    match action {
                        RebaseDialogAction::Confirm(branch, opts, target_dir) => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                view_state.pending_messages.push(
                                    AppMessage::RebaseBranchWithOptions(
                                        branch,
                                        opts.autostash,
                                        opts.rebase_merges,
                                        target_dir,
                                    ),
                                );
                            }
                        }
                        RebaseDialogAction::Cancel => {}
                    }
                    self.close_active_modal();
                }
            }

            ActiveModal::Settings => {
                self.settings_dialog
                    .handle_event(input_event, screen_bounds);
                if let Some(action) = self.settings_dialog.take_action() {
                    match action {
                        SettingsDialogAction::Close => {
                            self.apply_settings_changes();
                            self.close_active_modal();
                        }
                        SettingsDialogAction::ManageTokens => {
                            // Transition: Settings → TokenManager
                            self.settings_dialog.hide();
                            self.open_token_dialog();
                            // active_modal is now TokenManager (set by open_token_dialog)
                        }
                    }
                }
            }

            ActiveModal::TokenManager => {
                self.token_dialog.handle_event(input_event, screen_bounds);
                for action in self.token_dialog.take_actions() {
                    match action {
                        TokenDialogAction::Close => {
                            // Transition: TokenManager → Settings
                            self.token_dialog.hide();
                            self.settings_dialog.show();
                            self.active_modal = Some(ActiveModal::Settings);
                        }
                        other => self.handle_token_action(other),
                    }
                }
            }

            ActiveModal::RepoDialog => {
                self.repo_dialog.handle_event(input_event, screen_bounds);
                if !self.repo_dialog.is_visible() {
                    self.active_modal = None;
                }
            }

            ActiveModal::CloneDialog => {
                self.clone_dialog.handle_event(input_event, screen_bounds);
                if !self.clone_dialog.is_visible() {
                    self.active_modal = None;
                }
            }
        }

        true
    }

    /// Handle non-modal overlay events (toasts, context menus).
    fn handle_overlay_events(&mut self, input_event: &InputEvent, screen_bounds: Rect) -> bool {
        // Toast click-to-dismiss
        if self.toast_manager.handle_event(input_event, screen_bounds) {
            return true;
        }

        // Context menu
        if let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab)
            && view_state.context_menu.is_visible()
        {
            view_state
                .context_menu
                .handle_event(input_event, screen_bounds);
            if let Some(action) = view_state.context_menu.take_action() {
                match action {
                    MenuAction::Selected(action_id) => {
                        handle_context_menu_action(
                            &action_id,
                            view_state,
                            &mut self.toast_manager,
                            &mut self.confirm_dialog,
                            &mut self.branch_name_dialog,
                            &mut self.remote_dialog,
                            &mut self.merge_dialog,
                            &mut self.rebase_dialog,
                            &repo_tab.repo,
                            &mut self.pending_confirm_action,
                            &mut self.active_modal,
                        );
                    }
                }
            }
            return true;
        }

        false
    }

    /// Handle divider drag events (ongoing drag and starting new drags).
    /// Returns true if the event was consumed.
    fn handle_divider_drag(
        &mut self,
        input_event: &InputEvent,
        main_bounds: Rect,
        layout: &ScreenLayout,
    ) -> bool {
        // Handle ongoing drag (MouseMove / MouseUp) before anything else
        if self.divider_drag.is_some() {
            match input_event {
                InputEvent::MouseMove { x, y, .. } => {
                    let drag_kind = self.divider_drag.unwrap();
                    match drag_kind {
                        DividerDrag::SidebarGraph => {
                            let ratio = (*x - main_bounds.x) / main_bounds.width;
                            self.sidebar_ratio = ratio.clamp(0.05, 0.30);
                        }
                        DividerDrag::GraphRight => {
                            let sidebar_w =
                                main_bounds.width * self.sidebar_ratio.clamp(0.05, 0.30);
                            let content_x = main_bounds.x + sidebar_w;
                            let content_w = main_bounds.width - sidebar_w;
                            if content_w > 0.0 {
                                let ratio = (*x - content_x) / content_w;
                                self.graph_ratio = ratio.clamp(0.30, 0.80);
                            }
                        }
                        DividerDrag::StagingPreview => {
                            // Compute pill bar height to get content rect
                            if let Some((_, view_state)) = self.tabs.get(self.active_tab) {
                                let pill_bar_h = view_state
                                    .staging_well
                                    .pill_bar_height(&view_state.current_branch);
                                let (_, content_rect) = layout.right_panel.take_top(pill_bar_h);
                                if content_rect.height > 0.0 {
                                    let ratio = (*y - content_rect.y) / content_rect.height;
                                    self.staging_preview_ratio = ratio.clamp(0.30, 0.70);
                                }
                            }
                        }
                    }
                    if let Some(ref render_state) = self.state {
                        let cursor = match drag_kind {
                            DividerDrag::StagingPreview => CursorIcon::RowResize,
                            _ => CursorIcon::ColResize,
                        };
                        if self.current_cursor != cursor {
                            render_state.window.set_cursor(cursor);
                            self.current_cursor = cursor;
                        }
                    }
                    return true;
                }
                InputEvent::MouseUp { .. } => {
                    self.divider_drag = None;
                    if let Some(ref render_state) = self.state
                        && self.current_cursor != CursorIcon::Default
                    {
                        render_state.window.set_cursor(CursorIcon::Default);
                        self.current_cursor = CursorIcon::Default;
                    }
                    return true;
                }
                _ => {}
            }
        }

        // Start divider drag on MouseDown near divider edges (wide 8px hit zone)
        if let InputEvent::MouseDown {
            button: input::MouseButton::Left,
            x,
            y,
            ..
        } = input_event
        {
            let hit_tolerance = 8.0;

            if *y > layout.shortcut_bar.bottom() {
                let sidebar_edge = layout.sidebar.right();
                if (*x - sidebar_edge).abs() < hit_tolerance {
                    self.divider_drag = Some(DividerDrag::SidebarGraph);
                    return true;
                }

                let graph_edge = layout.graph.right();
                if (*x - graph_edge).abs() < hit_tolerance {
                    self.divider_drag = Some(DividerDrag::GraphRight);
                    return true;
                }

                // Horizontal divider: staging | preview (within right panel, staging mode only)
                if layout.right_panel.contains(*x, *y)
                    && let Some((_, view_state)) = self.tabs.get(self.active_tab)
                    && view_state.right_panel_mode == RightPanelMode::Staging
                {
                    let pill_bar_h = view_state
                        .staging_well
                        .pill_bar_height(&view_state.current_branch);
                    let (_, content_rect) = layout.right_panel.take_top(pill_bar_h);
                    let split_y = content_rect.y + content_rect.height * self.staging_preview_ratio;
                    if (*y - split_y).abs() < hit_tolerance {
                        self.divider_drag = Some(DividerDrag::StagingPreview);
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Handle global keyboard shortcuts (Ctrl+O, Ctrl+W, Ctrl+Tab, etc.).
    /// Returns true if the event was consumed.
    fn handle_global_shortcuts(&mut self, input_event: &InputEvent) -> bool {
        let InputEvent::KeyDown { key, modifiers, .. } = input_event else {
            return false;
        };

        // Ctrl+O: open repo
        if *key == Key::O && modifiers.only_ctrl() {
            self.repo_dialog.show_with_recent(&self.config.recent_repos);
            self.active_modal = Some(ActiveModal::RepoDialog);
            return true;
        }
        // Ctrl+Shift+O: clone repo
        if *key == Key::O && modifiers.ctrl && modifiers.shift && !modifiers.alt {
            let gh_token =
                token_store::get_github_token().or_else(|| self.config.github_token.clone());
            self.clone_dialog.show(gh_token.as_deref());
            self.active_modal = Some(ActiveModal::CloneDialog);
            return true;
        }
        // Ctrl+W: close tab
        if *key == Key::W && modifiers.only_ctrl() {
            if self.tabs.len() > 1 {
                let idx = self.active_tab;
                self.close_tab(idx);
            }
            return true;
        }
        // Ctrl+Tab: next tab
        if *key == Key::Tab && modifiers.only_ctrl() {
            let next = (self.active_tab + 1) % self.tabs.len();
            self.switch_tab(next);
            return true;
        }
        // Ctrl+Shift+Tab: previous tab
        if *key == Key::Tab && modifiers.ctrl_shift() {
            let prev = if self.active_tab == 0 {
                self.tabs.len() - 1
            } else {
                self.active_tab - 1
            };
            self.switch_tab(prev);
            return true;
        }
        // Ctrl+S: stash push (only when staging text inputs are not focused)
        if *key == Key::S
            && modifiers.only_ctrl()
            && let Some((_, view_state)) = self.tabs.get_mut(self.active_tab)
            && !view_state.staging_well.has_text_focus()
        {
            view_state.pending_messages.push(AppMessage::StashPush);
            return true;
        }
        // Ctrl+Shift+S: stash pop
        if *key == Key::S
            && modifiers.ctrl_shift()
            && let Some((_, view_state)) = self.tabs.get_mut(self.active_tab)
        {
            view_state.pending_messages.push(AppMessage::StashPop);
            return true;
        }
        // Ctrl+Shift+A: toggle amend mode
        if *key == Key::A
            && modifiers.ctrl_shift()
            && let Some((_, view_state)) = self.tabs.get_mut(self.active_tab)
            && !view_state.staging_well.has_text_focus()
        {
            view_state.pending_messages.push(AppMessage::ToggleAmend);
            return true;
        }
        // F5: diagnostic reload
        if *key == Key::F5 && !modifiers.any() {
            self.do_diagnostic_reload();
            return true;
        }
        // Ctrl+1..9: switch worktree context in staging well
        if modifiers.only_ctrl() {
            let wt_index = match *key {
                Key::Num1 => Some(0),
                Key::Num2 => Some(1),
                Key::Num3 => Some(2),
                Key::Num4 => Some(3),
                Key::Num5 => Some(4),
                Key::Num6 => Some(5),
                Key::Num7 => Some(6),
                Key::Num8 => Some(7),
                Key::Num9 => Some(8),
                _ => None,
            };
            if let Some(idx) = wt_index
                && let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab)
                && view_state.staging_well.has_worktree_selector()
                && idx < view_state.staging_well.worktree_count()
            {
                view_state.switch_to_worktree(idx, &repo_tab.repo);
                return true;
            }
        }
        // Ctrl+Shift+F: Fetch
        if *key == Key::F
            && modifiers.ctrl_shift()
            && let Some((_, view_state)) = self.tabs.get_mut(self.active_tab)
        {
            view_state.pending_messages.push(AppMessage::Fetch(None));
            return true;
        }
        // Ctrl+Shift+L: Pull
        if *key == Key::L
            && modifiers.ctrl_shift()
            && let Some((_, view_state)) = self.tabs.get_mut(self.active_tab)
        {
            let branch = view_state
                .current_branch_opt()
                .unwrap_or("HEAD")
                .to_string();
            view_state.pending_messages.push(AppMessage::Pull {
                remote: None,
                branch,
            });
            return true;
        }
        // Ctrl+Shift+P: Push
        if *key == Key::P
            && modifiers.ctrl_shift()
            && let Some((_, view_state)) = self.tabs.get_mut(self.active_tab)
        {
            let branch = view_state
                .current_branch_opt()
                .unwrap_or("HEAD")
                .to_string();
            view_state.pending_messages.push(AppMessage::Push {
                remote: None,
                branch,
            });
            return true;
        }
        // Ctrl+Shift+R: Pull --rebase
        if *key == Key::R
            && modifiers.ctrl_shift()
            && let Some((_, view_state)) = self.tabs.get_mut(self.active_tab)
        {
            let branch = view_state
                .current_branch_opt()
                .unwrap_or("HEAD")
                .to_string();
            view_state.pending_messages.push(AppMessage::PullRebase {
                remote: None,
                branch,
            });
            return true;
        }
        // Backtick (`): Open terminal at repo workdir
        if *key == Key::Grave
            && !modifiers.any()
            && let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab)
            && !view_state.staging_well.has_text_focus()
            && !view_state.branch_sidebar.has_text_focus()
        {
            let path = repo_tab.repo.git_command_dir();
            open_terminal_at(&path.to_string_lossy(), "repo", &mut self.toast_manager);
            return true;
        }

        false
    }

    /// Handle a sidebar action by dispatching to the appropriate pending message or dialog.
    fn handle_sidebar_action(&mut self, action: SidebarAction) {
        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };

        match action {
            SidebarAction::Checkout(name) => {
                view_state
                    .pending_messages
                    .push(AppMessage::CheckoutBranch(name));
            }
            SidebarAction::CheckoutRemote(remote, branch) => {
                view_state
                    .pending_messages
                    .push(AppMessage::CheckoutRemoteBranch(remote, branch));
            }
            SidebarAction::Delete(name) => {
                self.confirm_dialog
                    .show("Delete Branch", &format!("Delete local branch '{}'?", name));
                self.pending_confirm_action = Some(AppMessage::DeleteBranch(name));
                self.open_interrupt_modal(ActiveModal::Confirm);
            }
            SidebarAction::ApplyStash(index) => {
                view_state
                    .pending_messages
                    .push(AppMessage::StashApply(index));
            }
            SidebarAction::DropStash(index) => {
                self.confirm_dialog.show(
                    "Drop Stash",
                    &format!("Drop stash@{{{}}}? This cannot be undone.", index),
                );
                self.pending_confirm_action = Some(AppMessage::StashDrop(index));
                self.open_interrupt_modal(ActiveModal::Confirm);
            }
            SidebarAction::DeleteTag(name) => {
                self.confirm_dialog
                    .show("Delete Tag", &format!("Delete tag '{}'?", name));
                self.pending_confirm_action = Some(AppMessage::DeleteTag(name));
                self.open_interrupt_modal(ActiveModal::Confirm);
            }
            SidebarAction::SwitchWorktree(wt_name) => {
                view_state.switch_to_worktree_by_name(&wt_name, &repo_tab.repo);
            }
            SidebarAction::JumpToRef(ref_name) => {
                // Look up OID from branch tips or tags
                let oid = view_state
                    .commit_graph_view
                    .branch_tips
                    .iter()
                    .find(|t| t.name == ref_name)
                    .map(|t| t.oid)
                    .or_else(|| {
                        view_state
                            .commit_graph_view
                            .tags
                            .iter()
                            .find(|t| t.name == ref_name)
                            .map(|t| t.oid)
                    });
                if let Some(oid) = oid {
                    view_state
                        .pending_messages
                        .push(AppMessage::JumpToCommit(oid));
                }
            }
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
                let needs_rebuild = scale_factor > atlas_build_scale + 0.05;
                if needs_rebuild {
                    if std::env::var_os("WHISPER_TEXT_DIAG").is_some() {
                        eprintln!(
                            "text_diag scale_change: rebuilding atlas from {:.2} -> {:.2}",
                            atlas_build_scale, scale_factor
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
                // Refresh working directory status when dirty (watcher-driven) or on
                // a long safety-net interval (catches cases where the watcher misses
                // events, e.g. NFS, FUSE, or watcher init failure).
                {
                    let now = Instant::now();
                    if now.duration_since(self.last_status_refresh).as_secs() >= 30 {
                        self.status_dirty = true;
                    }
                    if self.status_dirty {
                        self.refresh_status();
                        self.status_dirty = false;
                        self.last_status_refresh = now;
                    }
                }

                // Periodic ref reconciliation: every 5s, check if branch tips / HEAD
                // changed externally (safety net for missed watcher events or libgit2 cache staleness)
                {
                    let now = Instant::now();
                    if now.duration_since(self.last_ref_check).as_secs() >= 5 {
                        self.last_ref_check = now;
                        if let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) {
                            let fresh = git::ref_fingerprint(repo_tab.repo.git_dir());
                            if fresh != 0 && fresh != view_state.ref_fingerprint {
                                view_state.ref_fingerprint = fresh;
                                // Refs changed — reopen handles and do full refresh
                                let _ = repo_tab.repo.reopen();
                                for wt_repo in view_state.worktree_state.repo_cache.values_mut() {
                                    let _ = wt_repo.reopen();
                                }
                                self.status_dirty = true;
                                self.trigger_repo_state_refresh();
                            }
                        }
                    }
                }

                // Periodic CI status polling
                if let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) {
                    let now = Instant::now();
                    // Skip if a fetch is already in flight
                    if view_state.ci_receivers.is_empty() {
                        // Determine poll interval:
                        // - 15s if CI is pending or within 5 min of a push
                        // - 5 min otherwise
                        let any_pending = view_state
                            .ci_results
                            .iter()
                            .any(|r| r.status.state == ci::CiState::Pending);
                        let fast_poll = any_pending
                            || view_state
                                .last_push_time
                                .is_some_and(|t| now.duration_since(t).as_secs() < 300);
                        let interval = if fast_poll { 15 } else { 300 };
                        let elapsed_since_fetch =
                            now.duration_since(view_state.last_ci_fetch).as_secs();

                        if elapsed_since_fetch >= interval {
                            trigger_ci_fetch(&self.config, repo_tab, view_state, &self.proxy);
                        }
                    }
                }

                // Poll native file picker for results
                self.repo_dialog.poll_picker();

                // Check for repo dialog actions
                if let Some(action) = self.repo_dialog.take_action() {
                    match action {
                        RepoDialogAction::Open(path) => {
                            let path_str = path.to_string_lossy().to_string();
                            if let Err(e) = self.config.add_recent_repo(&path_str) {
                                self.toast_manager.push(e, ToastSeverity::Error);
                            }
                            self.open_repo_tab(path);
                        }
                        RepoDialogAction::Cancel => {}
                    }
                }

                // Poll clone dialog
                self.clone_dialog.poll();

                if let Some(action) = self.clone_dialog.take_action() {
                    match action {
                        CloneDialogAction::Clone { url, dest, bare } => {
                            self.start_clone(url, dest, bare);
                        }
                        CloneDialogAction::Cancel => {}
                    }
                }

                // Poll clone receiver
                if let Some((ref rx, _started)) = self.clone_receiver {
                    match rx.try_recv() {
                        Ok(Ok(dest)) => {
                            self.clone_receiver = None;
                            let dest_str = dest.to_string_lossy().to_string();
                            self.toast_manager
                                .push(format!("Cloned into {}", dest_str), ToastSeverity::Success);
                            if let Err(e) = self.config.add_recent_repo(&dest_str) {
                                self.toast_manager.push(e, ToastSeverity::Error);
                            }
                            self.open_repo_tab(dest);
                        }
                        Ok(Err(err)) => {
                            self.clone_receiver = None;
                            self.toast_manager.push(
                                format!("Clone failed: {}", err.trim()),
                                ToastSeverity::Error,
                            );
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            self.clone_receiver = None;
                            self.toast_manager.push(
                                "Clone failed: background thread terminated".to_string(),
                                ToastSeverity::Error,
                            );
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    }
                }

                // Poll repo open receiver
                if let Some(ref rx) = self.open_receiver {
                    match rx.try_recv() {
                        Ok(Ok((repo, name))) => {
                            self.open_receiver = None;
                            let t = Instant::now();
                            self.finish_open_repo_tab(repo, name);
                            if frame_diag {
                                eprintln!(
                                    "[frame_diag] finish_open_repo_tab: {:.1}ms",
                                    t.elapsed().as_secs_f64() * 1000.0
                                );
                            }
                        }
                        Ok(Err(err)) => {
                            self.open_receiver = None;
                            self.toast_manager
                                .push(format!("Failed to open: {}", err), ToastSeverity::Error);
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            self.open_receiver = None;
                            self.toast_manager.push(
                                "Failed to open: background thread terminated".to_string(),
                                ToastSeverity::Error,
                            );
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    }
                }

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

        // Determine the next time we need to wake up
        let mut next_wake: Option<Instant> = None;
        let mut merge = |t: Instant| {
            next_wake = Some(next_wake.map_or(t, |w| w.min(t)));
        };

        // 1. Active animations (spinners, button pulse) → ~60fps
        if let Some((_, vs)) = self.tabs.get(self.active_tab) {
            let animating = vs.header_bar.fetching
                || vs.header_bar.pulling
                || vs.header_bar.pushing
                || vs.generic_op_receiver.is_some()
                || self.ai_commit_receiver.is_some();
            if animating {
                merge(self.last_frame_time + Duration::from_millis(16));
            }
        }

        // 2. Active toasts (fade animation)
        if self.toast_manager.has_active_toasts() {
            merge(self.last_frame_time + Duration::from_millis(16));
        }

        // 3. Cursor blink (530ms)
        if self.has_focused_text_input() {
            merge(self.last_frame_time + Duration::from_millis(530));
        }

        // 4. Status refresh timer (3s)
        merge(self.last_status_refresh + Duration::from_secs(3));

        // 5. Periodic ref reconciliation timer (5s)
        merge(self.last_ref_check + Duration::from_secs(5));

        // 6. Pending resize debounce
        if let Some(last_resize) = self.resize_debounce {
            merge(last_resize + Duration::from_millis(100));
        }

        // 7. CI status polling timer
        // Continue polling if we already have CI results (tokens may be in keychain, not config)
        if let Some((_, vs)) = self.tabs.get(self.active_tab)
            && vs.ci_receivers.is_empty()
            && !vs.ci_results.is_empty()
        {
            let any_pending = vs
                .ci_results
                .iter()
                .any(|r| r.status.state == ci::CiState::Pending);
            let fast_poll = any_pending
                || vs
                    .last_push_time
                    .is_some_and(|t| now.duration_since(t).as_secs() < 300);
            let interval = if fast_poll { 15 } else { 300 };
            merge(vs.last_ci_fetch + Duration::from_secs(interval));
        }

        match next_wake {
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

impl App {
    /// Check if any text input is currently focused (for cursor blink scheduling).
    fn has_focused_text_input(&self) -> bool {
        let Some((_, vs)) = self.tabs.get(self.active_tab) else {
            return false;
        };
        if vs.staging_well.subject_input.is_focused() {
            return true;
        }
        if vs.staging_well.body_area.is_focused() {
            return true;
        }
        if vs.branch_sidebar.has_text_focus() {
            return true;
        }
        if vs.commit_graph_view.search_bar.is_active() {
            return true;
        }
        if self.branch_name_dialog.is_visible() {
            return true;
        }
        if self.remote_dialog.is_visible() {
            return true;
        }
        if self.repo_dialog.is_visible() {
            return true;
        }
        if self.token_dialog.is_visible() {
            return true;
        }
        false
    }

    /// Dispatch an input event to the appropriate handler.
    fn handle_input_event(&mut self, event_loop: &ActiveEventLoop, input_event: &InputEvent) {
        let Some(ref state) = self.state else { return };

        // Calculate layout
        let extent = state.surface.extent();
        let screen_bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
        let scale = state.scale_factor as f32;
        let tab_bar_height = if self.tabs.len() > 1 {
            TabBar::height(scale)
        } else {
            0.0
        };
        let (tab_bar_bounds, main_bounds) = screen_bounds.take_top(tab_bar_height);
        let layout = ScreenLayout::compute_with_ratios_and_shortcut(
            main_bounds,
            4.0,
            scale,
            Some(self.sidebar_ratio),
            Some(self.graph_ratio),
            self.shortcut_bar_visible,
        );

        // Modal dialogs consume all events when visible
        if self.handle_modal_events(input_event, screen_bounds) {
            return;
        }

        // Divider drag handling
        if self.handle_divider_drag(input_event, main_bounds, &layout) {
            return;
        }

        // Global keyboard shortcuts
        if self.handle_global_shortcuts(input_event) {
            return;
        }

        // Route to tab bar (if visible)
        if self.tabs.len() > 1
            && self
                .tab_bar
                .handle_event(input_event, tab_bar_bounds)
                .is_consumed()
        {
            if let Some(action) = self.tab_bar.take_action() {
                match action {
                    TabAction::Select(idx) => self.switch_tab(idx),
                    TabAction::Close(idx) => self.close_tab(idx),
                    TabAction::New => {
                        self.repo_dialog.show_with_recent(&self.config.recent_repos);
                        self.active_modal = Some(ActiveModal::RepoDialog);
                    }
                }
            }
            return;
        }

        // Route to active tab's views
        let tab_count = self.tabs.len();
        let Some((_repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };

        // Handle per-tab global keys (except Tab, which is handled after panel routing)
        if let InputEvent::KeyDown { key, .. } = input_event
            && key == &Key::Escape
        {
            if view_state.right_panel_mode == RightPanelMode::Browse {
                view_state.right_panel_mode = RightPanelMode::Staging;
                view_state.commit_detail_view.clear();
                view_state.diff_view.clear();
                view_state.last_diff_commit = None;
            } else if view_state.diff_view.has_content() {
                view_state.diff_view.clear();
                view_state.last_diff_commit = None;
            } else if view_state.submodule_focus.is_some() {
                view_state.pending_messages.push(AppMessage::ExitSubmodule);
            } else {
                event_loop.exit();
            }
            return;
        }

        // Route to branch sidebar
        if view_state
            .branch_sidebar
            .handle_event(input_event, layout.sidebar)
            .is_consumed()
        {
            if matches!(input_event, InputEvent::MouseDown { .. }) {
                view_state.focused_panel = FocusedPanel::Sidebar;
                view_state.branch_sidebar.set_focused(true);
            }
            if let Some(action) = view_state.branch_sidebar.take_action() {
                self.handle_sidebar_action(action);
            }
            return;
        }

        // Route to header bar
        let mut do_reload = false;
        let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        if view_state
            .header_bar
            .handle_event(input_event, layout.header)
            .is_consumed()
        {
            if let Some(action) = view_state.header_bar.take_action() {
                use crate::ui::widgets::HeaderAction;
                match action {
                    HeaderAction::Fetch => {
                        view_state.pending_messages.push(AppMessage::Fetch(None));
                    }
                    HeaderAction::Pull => {
                        let branch = view_state
                            .current_branch_opt()
                            .unwrap_or("HEAD")
                            .to_string();
                        view_state.pending_messages.push(AppMessage::Pull {
                            remote: None,
                            branch,
                        });
                    }
                    HeaderAction::PullRebase => {
                        let branch = view_state
                            .current_branch_opt()
                            .unwrap_or("HEAD")
                            .to_string();
                        view_state.pending_messages.push(AppMessage::PullRebase {
                            remote: None,
                            branch,
                        });
                    }
                    HeaderAction::Push => {
                        let branch = view_state
                            .current_branch_opt()
                            .unwrap_or("HEAD")
                            .to_string();
                        view_state.pending_messages.push(AppMessage::Push {
                            remote: None,
                            branch,
                        });
                    }
                    HeaderAction::Help => {
                        self.shortcut_bar_visible = !self.shortcut_bar_visible;
                        self.config.shortcut_bar_visible = self.shortcut_bar_visible;
                        if let Err(e) = self.config.save() {
                            self.toast_manager.push(e, ToastSeverity::Error);
                        }
                    }
                    HeaderAction::Settings => {
                        self.set_active_modal(ActiveModal::Settings);
                    }
                    HeaderAction::BreadcrumbNav(depth) => {
                        view_state
                            .pending_messages
                            .push(AppMessage::ExitToDepth(depth));
                    }
                    HeaderAction::BreadcrumbClose => {
                        view_state.pending_messages.push(AppMessage::ExitToDepth(0));
                    }
                    HeaderAction::AbortOperation => {
                        view_state.pending_messages.push(AppMessage::AbortOperation);
                    }
                    HeaderAction::Reload => {
                        do_reload = true;
                    }
                    HeaderAction::OpenCiDetails(url) => {
                        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
                    }
                }
            }
            if do_reload {
                self.do_diagnostic_reload();
            }
            return;
        }

        // Route events to right panel content (commit detail + diff view)
        {
            let pill_bar_h = view_state
                .staging_well
                .pill_bar_height(&view_state.current_branch);
            let (_pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);

            // Route to commit detail view when in browse mode
            if view_state.right_panel_mode == RightPanelMode::Browse
                && view_state.commit_detail_view.has_content()
            {
                let (detail_rect, _diff_rect) = content_rect.split_vertical(0.40);
                if view_state
                    .commit_detail_view
                    .handle_event(input_event, detail_rect)
                    .is_consumed()
                {
                    if let Some(action) = view_state.commit_detail_view.take_action() {
                        match action {
                            CommitDetailAction::ViewFileDiff(oid, path) => {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::ViewCommitFileDiff(oid, path));
                            }
                            CommitDetailAction::OpenSubmodule(path_or_name) => {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::EnterSubmodule(path_or_name));
                            }
                        }
                    }
                    return;
                }
            }

            // Route events to diff view if it has content (both modes)
            // Skip keyboard events when staging well has text focus (commit message editing)
            if view_state.diff_view.has_content()
                && !(view_state.staging_well.has_text_focus()
                    && matches!(input_event, InputEvent::KeyDown { .. }))
            {
                let header_h = 28.0 * scale;
                let diff_bounds = match view_state.right_panel_mode {
                    RightPanelMode::Browse if view_state.commit_detail_view.has_content() => {
                        let (_detail_rect, diff_rect) = content_rect.split_vertical(0.40);
                        let (_hdr, body) = diff_rect.take_top(header_h);
                        body
                    }
                    RightPanelMode::Staging => {
                        let (_staging_rect, diff_rect) =
                            content_rect.split_vertical(self.staging_preview_ratio);
                        let (_hdr, body) = diff_rect.take_top(header_h);
                        body
                    }
                    _ => {
                        let (_hdr, body) = content_rect.take_top(header_h);
                        body
                    }
                };
                if view_state
                    .diff_view
                    .handle_event(input_event, diff_bounds)
                    .is_consumed()
                {
                    if let Some(action) = view_state.diff_view.take_action() {
                        match action {
                            DiffAction::Stage(path, hunk_idx) => {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::StageHunk(path, hunk_idx));
                            }
                            DiffAction::Unstage(path, hunk_idx) => {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::UnstageHunk(path, hunk_idx));
                            }
                            DiffAction::Discard(path, hunk_idx) => {
                                self.confirm_dialog.show(
                                    "Discard Hunk",
                                    "Discard this hunk? This cannot be undone.",
                                );
                                self.pending_confirm_action =
                                    Some(AppMessage::DiscardHunk(path, hunk_idx));
                                self.open_interrupt_modal(ActiveModal::Confirm);
                            }
                        }
                    }
                    return;
                }
            }
        }

        // Right-click context menus
        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        if let InputEvent::MouseDown {
            button: input::MouseButton::Right,
            x,
            y,
            ..
        } = input_event
        {
            // Check which panel was right-clicked and show context menu
            if layout.graph.contains(*x, *y) {
                if let Some((items, oid)) = view_state.commit_graph_view.context_menu_items_at(
                    *x,
                    *y,
                    &repo_tab.commits,
                    layout.graph,
                ) {
                    view_state.context_menu_commit = Some(oid);
                    view_state.context_menu.show(items, *x, *y);
                    return;
                }
            } else if layout.sidebar.contains(*x, *y) {
                if let Some(items) = view_state.branch_sidebar.context_menu_items_at(
                    *x,
                    *y,
                    layout.sidebar,
                    &view_state.current_branch,
                ) {
                    view_state.context_menu.show(items, *x, *y);
                    return;
                }
            } else if layout.right_panel.contains(*x, *y) {
                // Check pill bar first
                let pill_bar_h = view_state
                    .staging_well
                    .pill_bar_height(&view_state.current_branch);
                let (pill_rect, _) = layout.right_panel.take_top(pill_bar_h);
                if pill_rect.contains(*x, *y)
                    && let Some(items) = view_state.staging_well.pill_context_menu_at(*x, *y)
                {
                    view_state.context_menu.show(items, *x, *y);
                    return;
                }
                // Then check staging file lists
                if view_state.right_panel_mode == RightPanelMode::Staging {
                    let (_pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);
                    let (staging_rect, _) = content_rect.split_vertical(self.staging_preview_ratio);
                    if let Some(items) =
                        view_state
                            .staging_well
                            .context_menu_items_at(*x, *y, staging_rect)
                    {
                        view_state.context_menu.show(items, *x, *y);
                        return;
                    }
                }
            }
        }

        // Detect clicks on panels to switch focus
        if let InputEvent::MouseDown { x, y, .. } = input_event {
            if layout.right_panel.contains(*x, *y) {
                view_state.focused_panel = FocusedPanel::RightPanel;
            } else if layout.graph.contains(*x, *y) {
                view_state.focused_panel = FocusedPanel::Graph;
            } else if layout.sidebar.contains(*x, *y) {
                view_state.focused_panel = FocusedPanel::Sidebar;
                view_state.branch_sidebar.set_focused(true);
            }
            if view_state.focused_panel != FocusedPanel::Sidebar {
                view_state.branch_sidebar.set_focused(false);
            }
            if view_state.focused_panel != FocusedPanel::RightPanel
                || view_state.right_panel_mode != RightPanelMode::Staging
            {
                view_state.staging_well.unfocus_all();
            }
        }

        // Handle worktree pill bar clicks (before content routing)
        if let InputEvent::MouseDown { .. } = input_event {
            let pill_bar_h = view_state
                .staging_well
                .pill_bar_height(&view_state.current_branch);
            let (pill_rect, _content_rect) = layout.right_panel.take_top(pill_bar_h);
            if view_state
                .staging_well
                .handle_pill_event(input_event, pill_rect)
                .is_consumed()
            {
                if let Some(action) = view_state.staging_well.take_action()
                    && let StagingAction::SwitchWorktree(index) = action
                {
                    view_state.switch_to_worktree(index, &repo_tab.repo);
                }
                return;
            }
        }

        // Route scroll events to the panel under the mouse cursor (hover-based, not focus-based)
        if let InputEvent::Scroll { x, y, .. } = input_event {
            if layout.graph.contains(*x, *y) {
                let prev_selected = view_state.commit_graph_view.selected_commit;
                let response = view_state.commit_graph_view.handle_event(
                    input_event,
                    &repo_tab.commits,
                    layout.graph,
                    view_state.head_oid,
                );
                if view_state.commit_graph_view.selected_commit != prev_selected
                    && let Some(oid) = view_state.commit_graph_view.selected_commit
                    && view_state.last_diff_commit != Some(oid)
                {
                    if let Some(synthetic) = repo_tab
                        .commits
                        .iter()
                        .find(|c| c.id == oid && c.is_synthetic)
                    {
                        // Synthetic row: switch to that worktree if named
                        if let Some(wt_name) = synthetic.synthetic_wt_name.clone() {
                            view_state.switch_to_worktree_by_name(&wt_name, &repo_tab.repo);
                        } else {
                            // Single-worktree: enter staging mode directly
                            view_state.right_panel_mode = RightPanelMode::Staging;
                            view_state.last_diff_commit = None;
                            view_state.commit_detail_view.clear();
                            view_state.diff_view.clear();
                        }
                    } else {
                        view_state
                            .pending_messages
                            .push(AppMessage::SelectedCommit(oid));
                    }
                }
                if let Some(action) = view_state.commit_graph_view.take_action() {
                    view_state.handle_graph_action(action, &repo_tab.repo);
                }
                if response.is_consumed() {
                    return;
                }
            } else if layout.right_panel.contains(*x, *y)
                && view_state.right_panel_mode == RightPanelMode::Staging
            {
                let pill_bar_h = view_state
                    .staging_well
                    .pill_bar_height(&view_state.current_branch);
                let (_pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);
                let (staging_rect, _diff_rect) =
                    content_rect.split_vertical(self.staging_preview_ratio);
                let response = view_state
                    .staging_well
                    .handle_event(input_event, staging_rect);
                if let Some(action) = view_state.staging_well.take_action() {
                    view_state.handle_staging_action(action, &repo_tab.repo);
                }
                if response.is_consumed() {
                    return;
                }
            } else if layout.sidebar.contains(*x, *y) {
                // Sidebar scroll already routed above
            }
            // Scroll events should not fall through to focus-based routing
            return;
        }

        // Route to focused panel
        match view_state.focused_panel {
            FocusedPanel::Graph => {
                let prev_selected = view_state.commit_graph_view.selected_commit;
                let response = view_state.commit_graph_view.handle_event(
                    input_event,
                    &repo_tab.commits,
                    layout.graph,
                    view_state.head_oid,
                );
                if view_state.commit_graph_view.selected_commit != prev_selected
                    && let Some(oid) = view_state.commit_graph_view.selected_commit
                    && view_state.last_diff_commit != Some(oid)
                {
                    if let Some(synthetic) = repo_tab
                        .commits
                        .iter()
                        .find(|c| c.id == oid && c.is_synthetic)
                    {
                        if let Some(wt_name) = synthetic.synthetic_wt_name.clone() {
                            view_state.switch_to_worktree_by_name(&wt_name, &repo_tab.repo);
                        } else {
                            // Single-worktree: enter staging mode directly
                            view_state.right_panel_mode = RightPanelMode::Staging;
                            view_state.last_diff_commit = None;
                            view_state.commit_detail_view.clear();
                            view_state.diff_view.clear();
                        }
                    } else {
                        view_state
                            .pending_messages
                            .push(AppMessage::SelectedCommit(oid));
                    }
                }
                if let Some(action) = view_state.commit_graph_view.take_action() {
                    view_state.handle_graph_action(action, &repo_tab.repo);
                }
                if response.is_consumed() {
                    return;
                }
            }
            FocusedPanel::RightPanel => {
                if view_state.right_panel_mode == RightPanelMode::Staging {
                    let pill_bar_h = view_state
                        .staging_well
                        .pill_bar_height(&view_state.current_branch);
                    let (_pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);
                    let (staging_rect, _diff_rect) =
                        content_rect.split_vertical(self.staging_preview_ratio);
                    let response = view_state
                        .staging_well
                        .handle_event(input_event, staging_rect);

                    if let Some(action) = view_state.staging_well.take_action() {
                        view_state.handle_staging_action(action, &repo_tab.repo);
                    }
                    if response.is_consumed() {
                        return;
                    }
                }
                // Browse mode: diff/detail view events are handled above in pre-routing
            }
            FocusedPanel::Sidebar => {
                // Keyboard events handled by branch_sidebar.handle_event above
            }
        }

        // Tab to cycle panels (only when not consumed by focused panel)
        if let InputEvent::KeyDown { key: Key::Tab, .. } = input_event {
            view_state.focused_panel = match view_state.focused_panel {
                FocusedPanel::Graph => FocusedPanel::RightPanel,
                FocusedPanel::RightPanel => FocusedPanel::Sidebar,
                FocusedPanel::Sidebar => FocusedPanel::Graph,
            };
            view_state
                .branch_sidebar
                .set_focused(view_state.focused_panel == FocusedPanel::Sidebar);
            if view_state.focused_panel != FocusedPanel::RightPanel
                || view_state.right_panel_mode != RightPanelMode::Staging
            {
                view_state.staging_well.unfocus_all();
            }
            return;
        }

        // Update hover states
        if let InputEvent::MouseMove { x, y, .. } = input_event {
            view_state.header_bar.update_hover(*x, *y, layout.header);
            view_state
                .branch_sidebar
                .update_hover(*x, *y, layout.sidebar);
            {
                let pill_bar_h = view_state
                    .staging_well
                    .pill_bar_height(&view_state.current_branch);
                let (_pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);
                let (staging_rect, _diff_rect) =
                    content_rect.split_vertical(self.staging_preview_ratio);
                view_state.staging_well.update_hover(*x, *y, staging_rect);
            }

            if let Some(ref render_state) = self.state {
                if tab_count > 1 {
                    self.tab_bar.update_hover_with_renderer(
                        *x,
                        *y,
                        tab_bar_bounds,
                        &render_state.text_renderer,
                    );
                }

                let cursor = determine_cursor(
                    *x,
                    *y,
                    &layout,
                    view_state,
                    &self.tab_bar,
                    tab_count,
                    self.staging_preview_ratio,
                );
                if self.current_cursor != cursor {
                    render_state.window.set_cursor(cursor);
                    self.current_cursor = cursor;
                }
            }
        }
    }
}

/// Determine which cursor icon to show based on mouse position.
/// Returns resize cursors near divider edges, Pointer over clickable elements,
/// Text cursor over text inputs, Default otherwise.
fn determine_cursor(
    x: f32,
    y: f32,
    layout: &ScreenLayout,
    view_state: &TabViewState,
    tab_bar: &TabBar,
    tab_count: usize,
    staging_preview_ratio: f32,
) -> CursorIcon {
    // Wider hit zone for dividers (8px) makes dragging much easier
    let divider_hit = 8.0;

    // Check divider hover zones (only below shortcut bar)
    if y > layout.shortcut_bar.bottom() {
        // Divider 1: sidebar | graph (vertical)
        let sidebar_edge = layout.sidebar.right();
        if (x - sidebar_edge).abs() < divider_hit {
            return CursorIcon::ColResize;
        }

        // Divider 2: graph | right panel (vertical)
        let graph_edge = layout.graph.right();
        if (x - graph_edge).abs() < divider_hit {
            return CursorIcon::ColResize;
        }

        // Divider 3: staging | preview (horizontal, within right panel, staging mode only)
        if layout.right_panel.contains(x, y)
            && view_state.right_panel_mode == RightPanelMode::Staging
        {
            let pill_bar_h = view_state
                .staging_well
                .pill_bar_height(&view_state.current_branch);
            let (_, content_rect) = layout.right_panel.take_top(pill_bar_h);
            let split_y = content_rect.y + content_rect.height * staging_preview_ratio;
            if (y - split_y).abs() < divider_hit {
                return CursorIcon::RowResize;
            }
        }
    }

    // -- Text cursor: text input fields --

    // Staging area text inputs (subject line, body area) - only in staging mode
    if layout.right_panel.contains(x, y) && view_state.right_panel_mode == RightPanelMode::Staging {
        let pill_bar_h = view_state
            .staging_well
            .pill_bar_height(&view_state.current_branch); // scale already in pill_bar_height
        let (_pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);
        let (staging_rect, _diff_rect) = content_rect.split_vertical(staging_preview_ratio);
        let (_, _, subject_bounds, body_bounds, _) =
            view_state.staging_well.compute_regions(staging_rect);
        if subject_bounds.contains(x, y) || body_bounds.contains(x, y) {
            return CursorIcon::Text;
        }
    }

    // Search bar when active (overlays the graph area)
    if view_state.commit_graph_view.search_bar.is_active() && layout.graph.contains(x, y) {
        let scrollbar_width = theme::SCROLLBAR_WIDTH;
        let search_bar_height = 30.0;
        let search_bounds = Rect::new(
            layout.graph.x + 40.0,
            layout.graph.y + 4.0,
            layout.graph.width - 80.0 - scrollbar_width,
            search_bar_height,
        );
        if search_bounds.contains(x, y) {
            return CursorIcon::Text;
        }
    }

    // Sidebar filter bar (show text cursor when hovering the filter input area)
    if layout.sidebar.contains(x, y)
        && view_state
            .branch_sidebar
            .is_over_filter_bar(x, y, layout.sidebar)
    {
        return CursorIcon::Text;
    }

    // -- Pointer cursor: clickable elements --

    // Header bar buttons, breadcrumb links, abort button
    if layout.header.contains(x, y) && view_state.header_bar.is_any_interactive_hovered() {
        return CursorIcon::Pointer;
    }

    // Tab bar tabs, close buttons, new button
    if tab_count > 1 && tab_bar.is_any_hovered() {
        return CursorIcon::Pointer;
    }

    // Sidebar clickable items (branches, tags, stashes, etc.)
    if layout.sidebar.contains(x, y) && view_state.branch_sidebar.is_item_hovered() {
        return CursorIcon::Pointer;
    }

    // Staging well worktree pills (clickable to switch worktree)
    if layout.right_panel.contains(x, y) && view_state.staging_well.is_over_pill(x, y) {
        return CursorIcon::Pointer;
    }

    // Staging well buttons (Stage All, Unstage All, Commit, Amend)
    if layout.right_panel.contains(x, y) && view_state.staging_well.is_any_button_hovered() {
        return CursorIcon::Pointer;
    }

    // Staging well file list items (clickable to select/stage/unstage)
    if layout.right_panel.contains(x, y) && view_state.staging_well.is_file_hovered() {
        return CursorIcon::Pointer;
    }

    // Commit graph worktree/working pills (clickable to switch worktree)
    if layout.graph.contains(x, y) && view_state.commit_graph_view.hovered_pill {
        return CursorIcon::Pointer;
    }

    // Commit graph rows (clickable to select commit)
    if layout.graph.contains(x, y) && view_state.commit_graph_view.hovered_commit.is_some() {
        return CursorIcon::Pointer;
    }

    CursorIcon::Default
}
