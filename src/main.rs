//! Main application entry point and event loop.
//!
//! Owns the App struct, winit event loop, Vulkan draw pipeline, and three-layer rendering architecture
//! (base → chrome → overlay). Handles async git operations via mpsc channels and thread spawning.

mod ai;
mod config;
mod crash_log;
mod git;
mod github;
mod input;
mod messages;
mod renderer;
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
    Validated, VulkanError,
    command_buffer::{
        AutoCommandBufferBuilder, CommandBufferUsage, PrimaryAutoCommandBuffer, RenderPassBeginInfo,
    },
    format::{Format, NumericFormat},
    pipeline::graphics::viewport::Viewport,
    swapchain::{SwapchainPresentInfo, acquire_next_image},
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
use crate::git::{
    BranchTip, CommitInfo, GitRepo, RemoteOpResult, StashEntry, SubmoduleInfo, TagInfo,
    WorkingDirStatus, WorktreeInfo,
};
use crate::input::{InputEvent, InputState, Key};
use crate::messages::{
    AppMessage, MessageContext, MessageViewState, RepoStateSnapshot, RightPanelMode,
    compute_reload_deltas, handle_app_message,
};
use crate::renderer::{OffscreenTarget, SurfaceManager, VulkanContext, capture_to_buffer};
use crate::ui::widget::theme;
use crate::ui::widgets::{
    BranchNameDialog, BranchNameDialogAction, CloneDialog, CloneDialogAction, ConfirmDialog,
    ConfirmDialogAction, ContextMenu, HeaderBar, MenuAction, MenuItem, MergeDialog,
    MergeDialogAction, MergeStrategy, PullDialog, PullDialogAction, PushDialog, PushDialogAction,
    RebaseDialog, RebaseDialogAction, RemoteDialog, RemoteDialogAction, RepoDialog,
    RepoDialogAction, SettingsDialog, SettingsDialogAction, ShortcutBar, ShortcutContext,
    TabAction, TabBar, ToastManager, ToastSeverity, Tooltip,
};
use crate::ui::{
    AvatarCache, AvatarRenderer, IconRenderer, Rect, ScreenLayout, SplineRenderer, TextRenderer,
    Widget, WidgetOutput,
};
use crate::views::{
    BranchSidebar, CommitDetailAction, CommitDetailView, CommitGraphView, DiffAction, DiffView,
    GraphAction, SidebarAction, StagingAction, StagingWell,
};
use crate::watcher::{FsChangeKind, RepoWatcher};

/// Maximum number of commits to load into the graph view.
const MAX_COMMITS: usize = 50;

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
enum FocusedPanel {
    #[default]
    Graph,
    RightPanel,
    Sidebar,
}

/// Saved worktree state for submodule drill-down/restore.
struct SavedWorktreeState {
    worktrees: Vec<WorktreeInfo>,
    selected_path: Option<PathBuf>,
}

/// Consolidated worktree state for a tab.
/// Owns the worktree metadata list, the repo cache, and the user's selection.
struct WorktreeState {
    /// Worktree info from git (refreshed on repo state change)
    worktrees: Vec<WorktreeInfo>,
    /// Opened git2::Repository handles keyed by worktree path
    repo_cache: HashMap<PathBuf, GitRepo>,
    /// User's currently selected worktree (None = no worktree selected)
    selected_path: Option<PathBuf>,
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

    /// Refresh worktree list from repo, populate cache, prune stale entries.
    fn refresh(&mut self, repo: &GitRepo) {
        self.worktrees = repo.worktrees().unwrap_or_default();
        // Open repos for any new worktrees
        for wt in &self.worktrees {
            let path = PathBuf::from(&wt.path);
            if let std::collections::hash_map::Entry::Vacant(entry) = self.repo_cache.entry(path)
                && let Ok(wt_repo) = GitRepo::open(entry.key())
            {
                entry.insert(wt_repo);
            }
        }
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

struct SavedParentState {
    repo: GitRepo,
    commits: Vec<CommitInfo>,
    repo_name: String,
    graph_scroll_offset: f32,
    graph_top_row_index: usize,
    selected_commit: Option<Oid>,
    sidebar_scroll_offset: f32,
    submodule_name: String,
    parent_submodules: Vec<SubmoduleInfo>,
    worktree_state: SavedWorktreeState,
}

/// Focus state when viewing a submodule (supports nesting via stack)
struct SubmoduleFocus {
    parent_stack: Vec<SavedParentState>,
    current_name: String,
}

/// Per-tab repository data
struct RepoTab {
    repo: GitRepo,
    commits: Vec<CommitInfo>,
    name: String,
}

/// Per-tab UI view state
struct TabViewState {
    focused_panel: FocusedPanel,
    right_panel_mode: RightPanelMode,
    header_bar: HeaderBar,
    shortcut_bar: ShortcutBar,
    branch_sidebar: BranchSidebar,
    commit_graph_view: CommitGraphView,
    staging_well: StagingWell,
    diff_view: DiffView,
    commit_detail_view: CommitDetailView,
    context_menu: ContextMenu,
    /// Oid of the commit that was right-clicked for context menu
    context_menu_commit: Option<Oid>,
    last_diff_commit: Option<Oid>,
    pending_messages: Vec<AppMessage>,
    fetch_receiver: Option<(Receiver<RemoteOpResult>, Instant, String)>,
    pull_receiver: Option<(Receiver<RemoteOpResult>, Instant, String)>,
    push_receiver: Option<(Receiver<RemoteOpResult>, Instant, String)>,
    /// Generic async receiver for submodule/worktree ops (label for toast)
    generic_op_receiver: Option<(Receiver<RemoteOpResult>, String, Instant)>,
    /// Track whether we already showed the "still running" toast for each op
    showed_timeout_toast: [bool; 4],
    /// Consolidated worktree state: metadata, repo cache, and selection
    worktree_state: WorktreeState,
    /// Submodule drill-down state (None when viewing root repo)
    submodule_focus: Option<SubmoduleFocus>,
    /// Filesystem watcher for auto-refresh on external changes
    watcher: Option<RepoWatcher>,
    watcher_rx: Option<Receiver<FsChangeKind>>,
    /// Current branch name — derived from the active worktree's staging repo.
    /// Single source of truth; views read this instead of keeping their own copies.
    current_branch: String,
    /// HEAD commit OID — derived from the active worktree's staging repo.
    /// Single source of truth for the graph HEAD glow and Key::G jump.
    head_oid: Option<Oid>,
    /// Fingerprint of HEAD + local branch tip OIDs for periodic ref reconciliation.
    /// Compared every 5s to detect external ref changes missed by the watcher.
    ref_fingerprint: u64,
    /// Receiver for async GitHub CI status fetch
    ci_receiver: Option<Receiver<github::CiStatus>>,
    /// Most recent CI status for this tab's repo
    ci_status: Option<github::CiStatus>,
    /// When CI status was last fetched (for periodic polling)
    last_ci_fetch: Instant,
    /// When the last push completed (enables fast CI polling for 5 min)
    last_push_time: Option<Instant>,
    /// Branch that the current CI status corresponds to (detect branch switches)
    ci_branch: String,
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
            StagingAction::SwitchToSibling(name) => {
                // Exit current submodule, then enter the sibling
                self.pending_messages.push(AppMessage::ExitSubmodule);
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
            current_branch: String::new(),
            head_oid: None,
            ref_fingerprint: 0,
            ci_receiver: None,
            ci_status: None,
            last_ci_fetch: Instant::now(),
            last_push_time: None,
            ci_branch: String::new(),
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
        eprintln!("Event loop exited: {e}");
    }

    Ok(())
}

/// Which divider is currently being dragged
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DividerDrag {
    /// Vertical divider between sidebar and graph
    SidebarGraph,
    /// Vertical divider between graph and right panel
    GraphRight,
    /// Horizontal divider between staging and preview within the right panel
    StagingPreview,
}

struct App {
    cli_args: CliArgs,
    config: Config,
    tabs: Vec<(RepoTab, TabViewState)>,
    active_tab: usize,
    tab_bar: TabBar,
    repo_dialog: RepoDialog,
    clone_dialog: CloneDialog,
    settings_dialog: SettingsDialog,
    confirm_dialog: ConfirmDialog,
    branch_name_dialog: BranchNameDialog,
    remote_dialog: RemoteDialog,
    merge_dialog: MergeDialog,
    rebase_dialog: RebaseDialog,
    pull_dialog: PullDialog,
    push_dialog: PushDialog,
    pending_confirm_action: Option<AppMessage>,
    toast_manager: ToastManager,
    tooltip: Tooltip,
    state: Option<RenderState>,
    /// Which divider is currently being dragged, if any
    divider_drag: Option<DividerDrag>,
    /// Fraction of total width for sidebar (default ~0.14)
    sidebar_ratio: f32,
    /// Fraction of content width (after sidebar) for graph (default 0.55)
    graph_ratio: f32,
    /// Whether the shortcut bar is visible
    shortcut_bar_visible: bool,
    /// Fraction of right panel height for staging (default 0.45), remainder is preview
    staging_preview_ratio: f32,
    /// Current cursor icon (cached to avoid redundant Wayland protocol calls)
    current_cursor: CursorIcon,
    /// Dirty flag: true when refresh_status() should run on next frame
    status_dirty: bool,
    /// Timestamp of last refresh_status() call, for periodic refresh
    last_status_refresh: Instant,
    /// Receiver for async diff stats computation
    diff_stats_receiver: Option<Receiver<Vec<(Oid, usize, usize)>>>,
    /// Timestamp of app creation, for animation elapsed time
    app_start: Instant,
    /// Receiver for async AI commit message generation
    ai_commit_receiver: Option<(Receiver<Result<ai::AiResponse, String>>, Instant)>,
    /// AI provider resolved from config at startup
    ai_provider: ai::AiProvider,
    /// Proxy to wake the event loop from background threads
    proxy: EventLoopProxy<()>,
    /// Timestamp of the last frame render, for animation scheduling
    last_frame_time: Instant,
    /// Timestamp of the last periodic ref fingerprint check
    last_ref_check: Instant,
    /// Receiver for async git clone operation (success = destination path)
    clone_receiver: Option<(Receiver<Result<PathBuf, String>>, Instant)>,
    /// Receiver for async status refresh (background thread)
    status_receiver: Option<Receiver<StatusResult>>,
    /// Receiver for async repo state refresh (background thread)
    repo_state_receiver: Option<Receiver<RepoStateResult>>,
    /// "Before" snapshot for async diagnostic reload (F5). When Some, a diagnostic
    /// reload is in progress; finalized when both repo state and status results arrive.
    diagnostic_before: Option<RepoStateSnapshot>,
}

/// Initialized render state (after window creation) - shared across all tabs
struct RenderState {
    window: Arc<Window>,
    ctx: VulkanContext,
    surface: SurfaceManager,
    text_renderer: TextRenderer,
    bold_text_renderer: TextRenderer,
    spline_renderer: SplineRenderer,
    avatar_renderer: AvatarRenderer,
    avatar_cache: AvatarCache,
    icon_renderer: IconRenderer,
    previous_frame_end: Option<Box<dyn GpuFuture>>,
    frame_count: u32,
    scale_factor: f64,
    input_state: InputState,
}

fn rebuild_text_renderers(state: &mut RenderState, atlas_build_scale: f64) -> Result<()> {
    let mut upload_builder = AutoCommandBufferBuilder::primary(
        state.ctx.command_buffer_allocator.clone(),
        state.ctx.queue.queue_family_index(),
        CommandBufferUsage::OneTimeSubmit,
    )
    .context("Failed to create text upload command buffer")?;

    let mut text_renderer = TextRenderer::new(
        state.ctx.memory_allocator.clone(),
        state.surface.render_pass.clone(),
        &mut upload_builder,
        atlas_build_scale,
    )
    .context("Failed to rebuild text renderer")?;

    let mut bold_text_renderer = TextRenderer::new_bold(
        state.ctx.memory_allocator.clone(),
        state.surface.render_pass.clone(),
        &mut upload_builder,
        atlas_build_scale,
    )
    .context("Failed to rebuild bold text renderer")?;

    text_renderer.set_render_scale(state.scale_factor);
    bold_text_renderer.set_render_scale(state.scale_factor);

    let upload_cb = upload_builder
        .build()
        .context("Failed to build text upload command buffer")?;
    let upload_future = sync::now(state.ctx.device.clone())
        .then_execute(state.ctx.queue.clone(), upload_cb)
        .context("Failed to execute text upload")?
        .then_signal_fence_and_flush()
        .map_err(Validated::unwrap)
        .context("Failed to flush text upload")?;
    upload_future
        .wait(None)
        .context("Failed to wait for text upload")?;

    state.text_renderer = text_renderer;
    state.bold_text_renderer = bold_text_renderer;

    Ok(())
}

impl App {
    fn new(cli_args: CliArgs, proxy: EventLoopProxy<()>) -> Result<Self> {
        let config = Config::load();
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

        Ok(Self {
            cli_args,
            config,
            tabs,
            active_tab: 0,
            tab_bar,
            repo_dialog: RepoDialog::new(),
            clone_dialog: CloneDialog::new(),
            settings_dialog,
            confirm_dialog: ConfirmDialog::new(),
            branch_name_dialog: BranchNameDialog::new(),
            remote_dialog: RemoteDialog::new(),
            merge_dialog: MergeDialog::new(),
            rebase_dialog: RebaseDialog::new(),
            pull_dialog: PullDialog::new(),
            push_dialog: PushDialog::new(),
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
            last_ref_check: Instant::now(),
            clone_receiver: None,
            status_receiver: None,
            repo_state_receiver: None,
            diagnostic_before: None,
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

        // Create render pass with MSAA 4x
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
                },
            },
            pass: {
                color: [msaa_color],
                color_resolve: [resolve_target],
                depth_stencil: {},
            },
        )
        .context("Failed to create render pass")?;

        // Create surface manager
        let surface_mgr = SurfaceManager::new(&ctx, window.clone(), render_pass.clone())?;

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
                &mut self.toast_manager,
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
            let repo_git_dir = repo.git_dir().to_path_buf();
            let is_bare = repo.is_effectively_bare();

            // Determine staging repo git dir (worktree-specific or same as main)
            let staging_git_dir = view_state
                .worktree_state
                .staging_repo()
                .map(|r| r.git_dir().to_path_buf());

            let worktree_paths: Vec<String> = view_state
                .worktree_state
                .worktrees
                .iter()
                .map(|wt| wt.path.clone())
                .collect();

            self.status_receiver = Some(spawn_status_refresh(
                repo_git_dir,
                staging_git_dir,
                worktree_paths,
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
            let repo_git_dir = repo_tab.repo.git_dir().to_path_buf();
            let staging_git_dir = view_state
                .worktree_state
                .staging_repo()
                .map(|r| r.git_dir().to_path_buf());

            self.repo_state_receiver = Some(spawn_repo_state_refresh(
                repo_git_dir,
                staging_git_dir,
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
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.repo_state_receiver = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
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
                submodule_focus: &mut view_state.submodule_focus,
                proxy: self.proxy.clone(),
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
                submodule_focus: &mut view_state.submodule_focus,
                proxy: self.proxy.clone(),
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
                self.pull_dialog.show(branch, &default_remote);
                false
            } else if let AppMessage::ShowPushDialog(branch) = msg {
                let repo = &repo_tab.repo;
                let default_remote = repo
                    .default_remote()
                    .unwrap_or_else(|_| "origin".to_string());
                self.push_dialog.show(branch, &default_remote);
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
                                &mut self.toast_manager,
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
                                &mut self.toast_manager,
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
                submodule_focus: &mut view_state.submodule_focus,
                proxy: self.proxy.clone(),
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

        let github_token = self.config.github_token.clone();
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
                        trigger_ci_fetch(github_token.as_deref(), repo_tab, view_state, &proxy);
                    }
                    AsyncOpPoll::Failed(msg) => {
                        self.toast_manager.push(msg, ToastSeverity::Error);
                    }
                    AsyncOpPoll::Disconnected => {
                        self.toast_manager.push(
                            format!("{} failed: background thread terminated", $op_name),
                            ToastSeverity::Error,
                        );
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

        // Poll CI status receiver
        if let Some(ref rx) = view_state.ci_receiver
            && let Ok(status) = rx.try_recv()
        {
            view_state.ci_status = Some(status);
            view_state.ci_receiver = None;
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
                        self.toast_manager.push(msg, ToastSeverity::Error);
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    view_state.generic_op_receiver = None;
                    view_state.showed_timeout_toast[3] = false;
                    self.toast_manager.push(
                        format!("{} failed: background thread terminated", label),
                        ToastSeverity::Error,
                    );
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

    fn open_repo_tab(&mut self, path: PathBuf) {
        match GitRepo::open(path) {
            Ok(repo) => {
                let name = repo.repo_name();
                self.tab_bar.add_tab(name.clone());
                let mut view_state = TabViewState::new();

                // Initialize the view if render state exists
                // (init_tab_view -> refresh_repo_state will load commits)
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
                        &mut self.toast_manager,
                        self.config.show_orphaned_commits,
                        &self.proxy,
                    ));
                }

                trigger_ci_fetch(
                    self.config.github_token.as_deref(),
                    &repo_tab,
                    &mut view_state,
                    &self.proxy,
                );
                self.tabs.push((repo_tab, view_state));
                let new_idx = self.tabs.len() - 1;
                self.active_tab = new_idx;
                self.tab_bar.set_active(new_idx);
                self.toast_manager.push(
                    format!("Opened {}", self.tabs[new_idx].0.name),
                    ToastSeverity::Success,
                );
            }
            Err(e) => {
                self.toast_manager
                    .push(format!("Failed to open: {}", e), ToastSeverity::Error);
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
    fn handle_modal_events(&mut self, input_event: &InputEvent, screen_bounds: Rect) -> bool {
        // Confirm dialog takes highest modal priority
        if self.confirm_dialog.is_visible() {
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
            }
            return true;
        }

        // Branch name dialog takes modal priority
        if self.branch_name_dialog.is_visible() {
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
                    BranchNameDialogAction::CreateWorktree(name, source) => {
                        if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                            view_state
                                .pending_messages
                                .push(AppMessage::CreateWorktree(name, source));
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
            }
            return true;
        }

        // Remote dialog takes modal priority
        if self.remote_dialog.is_visible() {
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
            }
            return true;
        }

        // Pull dialog takes modal priority
        if self.pull_dialog.is_visible() {
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
            }
            return true;
        }

        // Push dialog takes modal priority
        if self.push_dialog.is_visible() {
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
            }
            return true;
        }

        // Merge dialog takes modal priority
        if self.merge_dialog.is_visible() {
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
                                    let commit_msg = message
                                        .unwrap_or_else(|| format!("Merge branch '{}'", branch));
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
            }
            return true;
        }

        // Rebase dialog takes modal priority
        if self.rebase_dialog.is_visible() {
            self.rebase_dialog.handle_event(input_event, screen_bounds);
            if let Some(action) = self.rebase_dialog.take_action() {
                match action {
                    RebaseDialogAction::Confirm(branch, opts, target_dir) => {
                        if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                            view_state
                                .pending_messages
                                .push(AppMessage::RebaseBranchWithOptions(
                                    branch,
                                    opts.autostash,
                                    opts.rebase_merges,
                                    target_dir,
                                ));
                        }
                    }
                    RebaseDialogAction::Cancel => {}
                }
            }
            return true;
        }

        // Settings dialog takes priority (modal)
        if self.settings_dialog.is_visible() {
            self.settings_dialog
                .handle_event(input_event, screen_bounds);
            if let Some(action) = self.settings_dialog.take_action() {
                match action {
                    SettingsDialogAction::Close => {
                        let row_scale = self.settings_dialog.row_scale;
                        let abbreviate_wt = self.settings_dialog.abbreviate_worktree_names;
                        let time_strength = self.settings_dialog.time_spacing_strength;
                        let fast_scroll = self.settings_dialog.scroll_speed >= 1.5;
                        let ratchet_scroll = self.settings_dialog.ratchet_scroll;
                        let orphans_changed = self.config.show_orphaned_commits
                            != self.settings_dialog.show_orphaned_commits;
                        if let Some(ref state) = self.state {
                            for (repo_tab, view_state) in &mut self.tabs {
                                view_state.commit_graph_view.row_scale = row_scale;
                                view_state.commit_graph_view.abbreviate_worktree_names =
                                    abbreviate_wt;
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
                        self.config.fast_scroll = self.settings_dialog.scroll_speed >= 1.5;
                        self.config.row_scale = self.settings_dialog.row_scale;
                        self.config.abbreviate_worktree_names =
                            self.settings_dialog.abbreviate_worktree_names;
                        self.config.time_spacing_strength =
                            self.settings_dialog.time_spacing_strength;
                        self.config.show_orphaned_commits =
                            self.settings_dialog.show_orphaned_commits;
                        self.config.ratchet_scroll = self.settings_dialog.ratchet_scroll;
                        if let Err(e) = self.config.save() {
                            self.toast_manager.push(e, ToastSeverity::Error);
                        }
                        // Reload commits if orphan visibility changed
                        if orphans_changed {
                            self.trigger_repo_state_refresh();
                        }
                    }
                }
            }
            return true;
        }

        // Repo dialog takes priority (modal)
        if self.repo_dialog.is_visible() {
            self.repo_dialog.handle_event(input_event, screen_bounds);
            return true;
        }

        // Clone dialog (modal)
        if self.clone_dialog.is_visible() {
            self.clone_dialog.handle_event(input_event, screen_bounds);
            return true;
        }

        // Toast click-to-dismiss (overlay, before context menu)
        if self.toast_manager.handle_event(input_event, screen_bounds) {
            return true;
        }

        // Context menu takes priority when visible (overlay)
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
            return true;
        }
        // Ctrl+Shift+O: clone repo
        if *key == Key::O && modifiers.ctrl && modifiers.shift && !modifiers.alt {
            self.clone_dialog.show(self.config.github_token.as_deref());
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
            }
            SidebarAction::DeleteTag(name) => {
                self.confirm_dialog
                    .show("Delete Tag", &format!("Delete tag '{}'?", name));
                self.pending_confirm_action = Some(AppMessage::DeleteTag(name));
            }
            SidebarAction::SwitchWorktree(wt_name) => {
                view_state.switch_to_worktree_by_name(&wt_name, &repo_tab.repo);
            }
        }
    }
}

/// Result of polling an async remote operation receiver.
enum AsyncOpPoll {
    /// Operation completed successfully; contains the remote/op name for the toast.
    Success(String),
    /// Operation failed; contains the classified error message.
    Failed(String),
    /// Background thread disconnected unexpectedly.
    Disconnected,
    /// Timeout threshold reached — caller should show a "still running" toast.
    Timeout,
    /// Still running, nothing to report yet.
    Pending,
}

/// Poll a remote operation receiver (fetch/pull/push) and return what happened.
/// On completion or disconnect, clears the receiver, header flag, and timeout flag.
/// On timeout, sets the timeout flag.
/// Trigger an async CI status fetch for the given tab, if a GitHub token is configured
/// and the origin remote is a GitHub URL.
fn trigger_ci_fetch(
    token: Option<&str>,
    repo_tab: &RepoTab,
    view_state: &mut TabViewState,
    proxy: &EventLoopProxy<()>,
) {
    if let Some(token) = token
        && !token.is_empty()
        && let Some(url) = repo_tab.repo.remote_url("origin")
    {
        let branch = view_state.current_branch_opt().map(|s| s.to_string());
        if let Some(rx) = github::fetch_ci_status_async(token, &url, branch.clone(), proxy.clone())
        {
            view_state.ci_receiver = Some(rx);
            view_state.last_ci_fetch = Instant::now();
            view_state.ci_branch = branch.unwrap_or_default();
        }
    }
}

fn poll_remote_op(
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
                AsyncOpPoll::Failed(msg)
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

/// Refresh commits, branch tips, tags, and header info from the repo.

// ============================================================================
// Async status refresh
// ============================================================================

/// Result of a background status refresh.
struct StatusResult {
    /// Main repo working directory status (for graph + header)
    main_status: Option<WorkingDirStatus>,
    /// Staging repo (worktree) status (for staging well)
    staging_status: Option<WorkingDirStatus>,
    /// Staging repo state (merge/rebase in progress, etc.)
    staging_repo_state: git2::RepositoryState,
    /// Per-worktree dirty flags: (path, is_dirty, dirty_file_count)
    worktree_dirty: Vec<(String, bool, usize)>,
    /// Pre-computed diff stats for the main repo working tree (insertions, deletions).
    /// Computed in the background thread to avoid blocking the main thread.
    main_diff_stats: Option<(usize, usize)>,
    /// Pre-computed diff stats for each dirty worktree: (path, insertions, deletions)
    worktree_diff_stats: Vec<(String, usize, usize)>,
    /// HEAD OID of the main repo (for synthetic entry parent linkage)
    head_oid: Option<Oid>,
    /// Workdir path of the main repo
    workdir: Option<String>,
}

/// Spawn a background thread to compute working directory status.
fn spawn_status_refresh(
    repo_git_dir: PathBuf,
    staging_git_dir: Option<PathBuf>,
    worktree_paths: Vec<String>,
    is_bare: bool,
    proxy: EventLoopProxy<()>,
) -> Receiver<StatusResult> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let main_repo = git2::Repository::open(&repo_git_dir).ok();

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

        let (staging_status, staging_repo_state) = match staging_git_dir
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
fn apply_status_result(
    result: StatusResult,
    repo_tab: &mut RepoTab,
    view_state: &mut TabViewState,
) {
    if let Some(status) = result.staging_status {
        view_state.staging_well.update_status(&status);
    }
    view_state.staging_well.repo_state_label =
        crate::git::repo_state_label(result.staging_repo_state);

    let main_dirty_count = result.main_status.as_ref().map(|s| s.total_files()).unwrap_or(0);
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
            if let (Some(head), Some(wd)) = (result.head_oid, &result.workdir) {
                if main_dirty_count > 0 {
                    let count = main_dirty_count;
                    let parent_time = repo_tab
                        .commits
                        .iter()
                        .find(|c| c.id == head)
                        .map(|c| c.time)
                        .unwrap_or(0);
                    let mut entry =
                        CommitInfo::synthetic_for_working_dir(head, count, wd, parent_time);
                    if let Some((ins, del)) = result.main_diff_stats {
                        entry.insertions = ins;
                        entry.deletions = del;
                    }
                    synthetics.push(entry);
                }
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
struct RepoStateResult {
    commits: Vec<CommitInfo>,
    branch_tips: Vec<BranchTip>,
    tags: Vec<TagInfo>,
    current_branch: String,
    head_oid: Option<Oid>,
    worktrees: Vec<WorktreeInfo>,
    remote_names: Vec<String>,
    remote_urls: HashMap<String, String>,
    is_bare: bool,
    submodules: Vec<SubmoduleInfo>,
    stashes: Vec<StashEntry>,
    ahead_behind: HashMap<String, (usize, usize)>,
    staging_repo_state: git2::RepositoryState,
    ref_fingerprint: u64,
    /// OIDs for which diff stats should be computed
    real_oids: Vec<Oid>,
    errors: Vec<String>,
}

/// Spawn a background thread to compute the full repo state refresh.
fn spawn_repo_state_refresh(
    repo_git_dir: PathBuf,
    staging_git_dir: Option<PathBuf>,
    show_orphaned_commits: bool,
    proxy: EventLoopProxy<()>,
) -> Receiver<RepoStateResult> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut errors = Vec::new();

        let repo = match GitRepo::open(&repo_git_dir) {
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
                    staging_repo_state: git2::RepositoryState::Clean,
                    ref_fingerprint: 0,
                    real_oids: Vec::new(),
                    errors,
                });
                let _ = proxy.send_event(());
                return;
            }
        };

        let staging_repo = staging_git_dir
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

        // Submodules
        let submodules = repo.submodules().unwrap_or_else(|e| {
            errors.push(format!("Failed to load submodules: {e}"));
            Vec::new()
        });

        // Stashes
        let stashes = repo.stash_list();

        // Ahead/behind for all branches
        let ahead_behind = repo.all_branches_ahead_behind();

        // Staging repo state
        let staging_repo_state = staging.repo_state();

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
            staging_repo_state,
            ref_fingerprint,
            real_oids,
            errors,
        });
        let _ = proxy.send_event(());
    });
    rx
}

/// Apply a completed repo state result to the UI.
/// Returns a diff stats receiver if OIDs are available.
fn apply_repo_state_result(
    result: RepoStateResult,
    repo_tab: &mut RepoTab,
    view_state: &mut TabViewState,
    toast_manager: &mut ToastManager,
    proxy: &EventLoopProxy<()>,
) -> Option<Receiver<Vec<(Oid, usize, usize)>>> {
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
    view_state
        .commit_graph_view
        .update_layout(&repo_tab.commits);
    view_state.commit_graph_view.branch_tips = result.branch_tips;
    view_state.commit_graph_view.tags = result.tags.clone();

    view_state.branch_sidebar.set_branch_data(
        &view_state.commit_graph_view.branch_tips,
        &result.tags,
        &result.remote_names,
        &result.remote_urls,
        &result.worktrees,
        result.is_bare,
    );
    view_state.staging_well.set_worktrees(&result.worktrees);

    // Update worktree state
    view_state.worktree_state.worktrees = result.worktrees;
    // Open repos for any new worktrees in the cache
    for wt in &view_state.worktree_state.worktrees {
        let path = PathBuf::from(&wt.path);
        if let std::collections::hash_map::Entry::Vacant(entry) =
            view_state.worktree_state.repo_cache.entry(path)
            && let Ok(wt_repo) = GitRepo::open(entry.key())
        {
            entry.insert(wt_repo);
        }
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

    view_state.staging_well.set_submodules(result.submodules);

    // Sibling submodules for lateral navigation
    if let Some(focus) = view_state.submodule_focus.as_ref() {
        if let Some(parent) = focus.parent_stack.last() {
            view_state
                .staging_well
                .set_sibling_submodules(parent.parent_submodules.clone());
        }
    } else {
        view_state.staging_well.sibling_submodules.clear();
    }

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
    view_state.header_bar.operation_state_label = git::repo_state_label(result.staging_repo_state);

    // Derive HEAD from worktree state
    view_state.current_branch = result.current_branch;
    view_state.head_oid = result.head_oid;
    for tip in &mut view_state.commit_graph_view.branch_tips {
        tip.is_head = !tip.is_remote && tip.name == view_state.current_branch;
    }

    // Update ref fingerprint
    view_state.ref_fingerprint = result.ref_fingerprint;

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

/// Initialize a tab's view state from its repo data
fn init_tab_view(
    repo_tab: &mut RepoTab,
    view_state: &mut TabViewState,
    text_renderer: &TextRenderer,
    scale: f32,
    toast_manager: &mut ToastManager,
    show_orphaned_commits: bool,
    proxy: &EventLoopProxy<()>,
) -> Receiver<RepoStateResult> {
    // Sync view metrics to the current text renderer scale
    view_state.commit_graph_view.sync_metrics(text_renderer);
    view_state.branch_sidebar.sync_metrics(text_renderer);
    view_state.staging_well.set_scale(scale);

    // Set initial repo path in header — use common_dir parent to show project path,
    // not a worktree-specific path.
    let project_path = repo_tab
        .repo
        .common_dir()
        .parent()
        .unwrap_or(repo_tab.repo.common_dir());
    let repo_path_str = project_path.to_string_lossy().into_owned();
    let repo_path_str = repo_path_str.trim_end_matches('/').to_string();
    view_state.header_bar.set_repo_path(&repo_path_str);

    // No worktree is auto-selected at init. The user picks one via the staging well selector.
    // worktree_state.selected_path stays None until explicitly set.

    // Spawn async repo state refresh
    let repo_git_dir = repo_tab.repo.git_dir().to_path_buf();
    let rx = spawn_repo_state_refresh(repo_git_dir, None, show_orphaned_commits, proxy.clone());

    // Start filesystem watcher for auto-refresh
    start_watcher(repo_tab, view_state, toast_manager, proxy);

    rx
}

/// Start (or restart) a filesystem watcher for the given tab's repo.
fn start_watcher(
    repo_tab: &RepoTab,
    view_state: &mut TabViewState,
    toast_manager: &mut ToastManager,
    proxy: &EventLoopProxy<()>,
) {
    // Drop any existing watcher first
    view_state.watcher = None;
    view_state.watcher_rx = None;

    let repo = &repo_tab.repo;
    let Some(workdir) = repo.workdir() else {
        return;
    };
    let git_dir = repo.git_dir();
    let common_dir = repo.common_dir();

    match RepoWatcher::new(
        workdir,
        git_dir,
        common_dir,
        &view_state.worktree_state.worktrees,
        proxy.clone(),
    ) {
        Ok((watcher, rx)) => {
            view_state.watcher = Some(watcher);
            view_state.watcher_rx = Some(rx);
        }
        Err(e) => {
            toast_manager.push(
                format!("Filesystem watcher failed: {}", e),
                ToastSeverity::Error,
            );
        }
    }
}

/// Drill into a named submodule: saves parent state and swaps repo to the submodule.
/// Returns true on success.
#[allow(clippy::too_many_arguments)]
fn enter_submodule(
    name: &str,
    repo_tab: &mut RepoTab,
    view_state: &mut TabViewState,
    text_renderer: &TextRenderer,
    scale: f32,
    toast_manager: &mut ToastManager,
    show_orphaned_commits: bool,
    proxy: &EventLoopProxy<()>,
) -> Option<Receiver<RepoStateResult>> {
    // Find the submodule info by name
    let sm = view_state
        .staging_well
        .submodules
        .iter()
        .find(|s| s.name == name)
        .cloned();
    let Some(sm) = sm else {
        toast_manager.push(
            format!("Submodule '{}' not found", name),
            ToastSeverity::Error,
        );
        return None;
    };

    // Resolve submodule path relative to the active worktree's workdir
    let parent_workdir = match view_state.staging_well.active_worktree_path() {
        Some(path) => path,
        None => {
            toast_manager.push("No active worktree".to_string(), ToastSeverity::Error);
            return None;
        }
    };
    let sub_path = parent_workdir.join(sm.path);

    // Open the submodule as a repo
    let sub_repo = match GitRepo::open(sub_path) {
        Ok(r) => r,
        Err(e) => {
            toast_manager.push(
                format!("Cannot open submodule '{}': {}", name, e),
                ToastSeverity::Error,
            );
            return None;
        }
    };

    // Save parent state — use std::mem::replace for atomic swap (repo is non-optional)
    let parent_repo = std::mem::replace(&mut repo_tab.repo, sub_repo);
    let parent_commits = std::mem::take(&mut repo_tab.commits);
    let parent_name = repo_tab.name.clone();
    let parent_submodules = view_state.staging_well.submodules.clone();

    let saved = SavedParentState {
        repo: parent_repo,
        commits: parent_commits,
        repo_name: parent_name,
        graph_scroll_offset: view_state.commit_graph_view.scroll_offset,
        graph_top_row_index: view_state.commit_graph_view.top_row_index,
        selected_commit: view_state.commit_graph_view.selected_commit,
        sidebar_scroll_offset: view_state.branch_sidebar.scroll_offset,
        submodule_name: name.to_string(),
        parent_submodules,
        worktree_state: view_state.worktree_state.save(),
    };

    // Clear diff/detail views and worktree state for the submodule
    view_state.diff_view.clear();
    view_state.commit_detail_view.clear();
    view_state.last_diff_commit = None;
    view_state.worktree_state = WorktreeState::new();

    // Clear staging well immediately to avoid showing stale parent files
    view_state.staging_well.clear_status();

    // Swap in submodule data (repo already swapped via std::mem::replace above)
    let sub_commits = repo_tab.repo.commit_graph(MAX_COMMITS).unwrap_or_default();
    repo_tab.name = name.to_string();
    repo_tab.commits = sub_commits;

    // Build/extend focus state
    match &mut view_state.submodule_focus {
        Some(focus) => {
            focus.parent_stack.push(saved);
            focus.current_name = name.to_string();
        }
        None => {
            view_state.submodule_focus = Some(SubmoduleFocus {
                parent_stack: vec![saved],
                current_name: name.to_string(),
            });
        }
    }

    // Re-init views with the submodule data
    let rx = init_tab_view(
        repo_tab,
        view_state,
        text_renderer,
        scale,
        toast_manager,
        show_orphaned_commits,
        proxy,
    );

    Some(rx)
}

/// Pop one level from the submodule focus stack, restoring parent state.
/// Returns a receiver for the async repo state refresh, or None on failure.
fn exit_submodule(
    repo_tab: &mut RepoTab,
    view_state: &mut TabViewState,
    text_renderer: &TextRenderer,
    scale: f32,
    toast_manager: &mut ToastManager,
    show_orphaned_commits: bool,
    proxy: &EventLoopProxy<()>,
) -> Option<Receiver<RepoStateResult>> {
    // Pop saved state from the focus stack (release borrow before init_tab_view)
    let saved = {
        let focus = view_state.submodule_focus.as_mut()?;
        focus.parent_stack.pop()?
    };

    // Clear diff/detail
    view_state.diff_view.clear();
    view_state.commit_detail_view.clear();
    view_state.last_diff_commit = None;

    // Restore parent data
    let scroll_offset = saved.graph_scroll_offset;
    let top_row_index = saved.graph_top_row_index;
    let selected = saved.selected_commit;
    let sidebar_scroll = saved.sidebar_scroll_offset;
    let parent_submodules = saved.parent_submodules;

    repo_tab.repo = saved.repo;
    repo_tab.commits = saved.commits;
    repo_tab.name = saved.repo_name;
    view_state
        .worktree_state
        .restore(saved.worktree_state, &repo_tab.repo);

    // Re-init views with parent data
    let rx = init_tab_view(
        repo_tab,
        view_state,
        text_renderer,
        scale,
        toast_manager,
        show_orphaned_commits,
        proxy,
    );

    // Restore scroll/selection
    view_state.commit_graph_view.scroll_offset = scroll_offset;
    view_state.commit_graph_view.top_row_index = top_row_index;
    view_state.commit_graph_view.selected_commit = selected;
    view_state.branch_sidebar.scroll_offset = sidebar_scroll;

    // Restore submodule siblings in staging well
    view_state.staging_well.set_submodules(parent_submodules);

    // If stack is now empty, clear focus entirely
    let stack_empty = view_state
        .submodule_focus
        .as_ref()
        .map(|f| f.parent_stack.is_empty())
        .unwrap_or(true);
    if stack_empty {
        view_state.submodule_focus = None;
    } else if let Some(ref mut focus) = view_state.submodule_focus {
        // Update current_name to the parent that's now active
        focus.current_name = focus
            .parent_stack
            .last()
            .map(|s| s.submodule_name.clone())
            .unwrap_or_default();
    }

    Some(rx)
}

/// Pop multiple levels to reach the given depth (0 = root).
#[allow(clippy::too_many_arguments)]
fn exit_to_depth(
    depth: usize,
    repo_tab: &mut RepoTab,
    view_state: &mut TabViewState,
    text_renderer: &TextRenderer,
    scale: f32,
    toast_manager: &mut ToastManager,
    show_orphaned_commits: bool,
    proxy: &EventLoopProxy<()>,
) -> Option<Receiver<RepoStateResult>> {
    let current_depth = view_state
        .submodule_focus
        .as_ref()
        .map(|f| f.parent_stack.len())
        .unwrap_or(0);
    if depth >= current_depth {
        return None;
    }
    let pops = current_depth - depth;
    let mut last_rx = None;
    for _ in 0..pops {
        match exit_submodule(
            repo_tab,
            view_state,
            text_renderer,
            scale,
            toast_manager,
            show_orphaned_commits,
            proxy,
        ) {
            Some(rx) => last_rx = Some(rx),
            None => break,
        }
    }
    last_rx
}

/// Try to open a terminal emulator at the given directory path.
/// Checks $TERMINAL env var first, then falls back to common terminals.
fn open_terminal_at(dir: &str, label: &str, toast_manager: &mut ToastManager) {
    use std::process::Command;

    let path = std::path::Path::new(dir);
    if !path.exists() {
        toast_manager.push(
            format!("Path does not exist: {}", dir),
            ToastSeverity::Error,
        );
        return;
    }

    // Check $TERMINAL env var first, then try common terminal emulators
    let candidates: Vec<String> = if let Ok(term) = std::env::var("TERMINAL") {
        std::iter::once(term)
            .chain(
                [
                    "kitty",
                    "alacritty",
                    "wezterm",
                    "foot",
                    "xterm",
                    "gnome-terminal",
                    "konsole",
                ]
                .iter()
                .map(|s| s.to_string()),
            )
            .collect()
    } else {
        [
            "kitty",
            "alacritty",
            "wezterm",
            "foot",
            "xterm",
            "gnome-terminal",
            "konsole",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    };

    for terminal in &candidates {
        let result = if terminal == "gnome-terminal" {
            Command::new(terminal)
                .arg("--working-directory")
                .arg(dir)
                .spawn()
        } else {
            // Most terminals accept --working-directory or use the cwd
            Command::new(terminal).current_dir(dir).spawn()
        };

        match result {
            Ok(_) => {
                toast_manager.push(
                    format!("Opened {} in {}", label, terminal),
                    ToastSeverity::Success,
                );
                return;
            }
            Err(_) => continue,
        }
    }

    toast_manager.push(
        "No terminal emulator found. Set $TERMINAL env var.".to_string(),
        ToastSeverity::Info,
    );
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
                state.surface.needs_recreate = true;
                state.window.request_redraw();
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                state.scale_factor = scale_factor;
                let atlas_build_scale = state.text_renderer.atlas_build_scale() as f64;
                let needs_rebuild = scale_factor > atlas_build_scale + 0.05;
                if needs_rebuild {
                    if std::env::var_os("WHISPER_TEXT_DIAG").is_some() {
                        eprintln!(
                            "text_diag scale_change: rebuilding atlas from {:.2} -> {:.2}",
                            atlas_build_scale, scale_factor
                        );
                    }
                    if let Err(e) = rebuild_text_renderers(state, scale_factor) {
                        eprintln!("Failed to rebuild text atlases: {e:?}");
                        state.text_renderer.set_render_scale(scale_factor);
                        state.bold_text_renderer.set_render_scale(scale_factor);
                    }
                } else {
                    state.text_renderer.set_render_scale(scale_factor);
                    state.bold_text_renderer.set_render_scale(scale_factor);
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

                // Poll async diff stats FIRST — apply completed results before
                // watcher or remote ops can orphan the receiver with a new one
                self.poll_diff_stats();
                // Re-launch diff stats if the previous receiver was orphaned
                self.ensure_diff_stats();
                // Poll background status refresh
                self.poll_status();
                // Poll background repo state refresh
                self.poll_repo_state();
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
                self.process_messages();
                // Check if staging well requested an immediate status refresh (e.g., worktree switch)
                if let Some((_rt, vs)) = self.tabs.get_mut(self.active_tab)
                    && vs.staging_well.status_refresh_needed
                {
                    self.status_dirty = true;
                    vs.staging_well.status_refresh_needed = false;
                }
                // Refresh working directory status only when dirty or on a periodic timer
                {
                    let now = Instant::now();
                    if now.duration_since(self.last_status_refresh).as_millis() >= 3000 {
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
                    if view_state.ci_receiver.is_none() {
                        let branch_changed = view_state
                            .current_branch_opt()
                            .is_some_and(|b| b != view_state.ci_branch);

                        // Determine poll interval:
                        // - 15s if CI is pending or within 5 min of a push
                        // - 5 min otherwise
                        let fast_poll = view_state
                            .ci_status
                            .as_ref()
                            .is_some_and(|s| s.state == github::CiState::Pending)
                            || view_state
                                .last_push_time
                                .is_some_and(|t| now.duration_since(t).as_secs() < 300);
                        let interval = if fast_poll { 15 } else { 300 };
                        let elapsed_since_fetch =
                            now.duration_since(view_state.last_ci_fetch).as_secs();

                        if branch_changed || elapsed_since_fetch >= interval {
                            trigger_ci_fetch(
                                self.config.github_token.as_deref(),
                                repo_tab,
                                view_state,
                                &self.proxy,
                            );
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

                if let Err(e) = draw_frame(self) {
                    crash_log::breadcrumb(format!("draw_frame error: {e:?}"));
                    eprintln!("Draw error: {e:?}");
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

        // 6. CI status polling timer
        if let Some((_, vs)) = self.tabs.get(self.active_tab)
            && vs.ci_receiver.is_none()
            && self
                .config
                .github_token
                .as_ref()
                .is_some_and(|t| !t.is_empty())
        {
            let fast_poll = vs
                .ci_status
                .as_ref()
                .is_some_and(|s| s.state == github::CiState::Pending)
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
                    TabAction::New => self.repo_dialog.show_with_recent(&self.config.recent_repos),
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
                        self.settings_dialog.show();
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
                            CommitDetailAction::OpenSubmodule(name) => {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::EnterSubmodule(name));
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

// ============================================================================
// Rendering
// ============================================================================

/// Render the preview/diff panel header bar (SURFACE_RAISED background + bold title).
/// Returns the body rect below the header.
fn render_preview_header(
    output: &mut WidgetOutput,
    rect: Rect,
    title: &str,
    is_placeholder: bool,
    scale: f32,
    bold_text_renderer: &TextRenderer,
) -> Rect {
    let header_h = 28.0 * scale;
    let (header_rect, body_rect) = rect.take_top(header_h);
    output
        .spline_vertices
        .extend(crate::ui::widget::create_rect_vertices(
            &header_rect,
            theme::SURFACE_RAISED.to_array(),
        ));
    let header_text_y = header_rect.y + (header_h - bold_text_renderer.line_height()) / 2.0;
    let header_text_x = header_rect.x + 12.0 * scale;
    let color = if is_placeholder {
        theme::TEXT_MUTED
    } else {
        theme::TEXT_BRIGHT
    };
    output
        .bold_text_vertices
        .extend(bold_text_renderer.layout_text(
            title,
            header_text_x,
            header_text_y,
            color.to_array(),
        ));
    body_rect
}

/// Handle a context menu action by dispatching to the appropriate AppMessage
#[allow(clippy::too_many_arguments)]
fn handle_context_menu_action(
    action_id: &str,
    view_state: &mut TabViewState,
    toast_manager: &mut ToastManager,
    confirm_dialog: &mut ConfirmDialog,
    branch_name_dialog: &mut BranchNameDialog,
    remote_dialog: &mut RemoteDialog,
    merge_dialog: &mut MergeDialog,
    rebase_dialog: &mut RebaseDialog,
    repo: &crate::git::GitRepo,
    pending_confirm_action: &mut Option<AppMessage>,
) {
    // Actions may be in format "action:param" or just "action"
    let (action, param) = action_id.split_once(':').unwrap_or((action_id, ""));

    match action {
        // Commit graph actions
        "copy_sha" => {
            if let Some(oid) = view_state.context_menu_commit {
                let sha = oid.to_string();
                match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(&sha)) {
                    Ok(()) => {
                        toast_manager
                            .push(format!("Copied: {}", &sha[..7]), ToastSeverity::Success);
                    }
                    Err(e) => {
                        toast_manager.push(format!("Clipboard error: {e}"), ToastSeverity::Error);
                    }
                }
            }
        }
        "view_details" => {
            if let Some(oid) = view_state.context_menu_commit {
                view_state
                    .pending_messages
                    .push(AppMessage::SelectedCommit(oid));
            }
        }
        "checkout" => {
            if param.is_empty() {
                // Commit graph checkout: find the branch at the selected commit
                if let Some(oid) = view_state.context_menu_commit
                    && let Some(tip) = view_state
                        .commit_graph_view
                        .branch_tips
                        .iter()
                        .find(|t| t.oid == oid && !t.is_remote)
                {
                    view_state
                        .pending_messages
                        .push(AppMessage::CheckoutBranch(tip.name.clone()));
                }
            } else {
                // Branch sidebar checkout
                view_state
                    .pending_messages
                    .push(AppMessage::CheckoutBranch(param.to_string()));
            }
        }
        "checkout_remote" => {
            if let Some((remote, branch)) = param.split_once('/') {
                view_state
                    .pending_messages
                    .push(AppMessage::CheckoutRemoteBranch(
                        remote.to_string(),
                        branch.to_string(),
                    ));
            }
        }
        "rename" => {
            if !param.is_empty() {
                branch_name_dialog.show_for_rename(param);
            }
        }
        "delete" => {
            if !param.is_empty() {
                confirm_dialog.show(
                    "Delete Branch",
                    &format!("Delete local branch '{}'?", param),
                );
                *pending_confirm_action = Some(AppMessage::DeleteBranch(param.to_string()));
            }
        }
        "push" => {
            let branch = if param.is_empty() {
                view_state
                    .current_branch_opt()
                    .unwrap_or("HEAD")
                    .to_string()
            } else {
                param.to_string()
            };
            view_state.pending_messages.push(AppMessage::Push {
                remote: None,
                branch,
            });
        }
        "push_to" => {
            view_state
                .pending_messages
                .push(AppMessage::ShowPushDialog(param.to_string()));
        }
        "pull" => {
            let branch = if param.is_empty() {
                view_state
                    .current_branch_opt()
                    .unwrap_or("HEAD")
                    .to_string()
            } else {
                param.to_string()
            };
            view_state.pending_messages.push(AppMessage::Pull {
                remote: None,
                branch,
            });
        }
        "pull_rebase" => {
            let branch = if param.is_empty() {
                view_state
                    .current_branch_opt()
                    .unwrap_or("HEAD")
                    .to_string()
            } else {
                param.to_string()
            };
            view_state.pending_messages.push(AppMessage::PullRebase {
                remote: None,
                branch,
            });
        }
        "pull_from_dialog" => {
            view_state
                .pending_messages
                .push(AppMessage::ShowPullDialog(param.to_string()));
        }
        "force_push" => {
            let branch = if param.is_empty() {
                view_state
                    .current_branch_opt()
                    .unwrap_or("HEAD")
                    .to_string()
            } else {
                param.to_string()
            };
            confirm_dialog.show(
                "Force Push",
                &format!(
                    "Force push '{}' with --force-with-lease? This may overwrite remote commits.",
                    branch
                ),
            );
            *pending_confirm_action = Some(AppMessage::PushForce {
                remote: None,
                branch,
            });
        }
        "fetch_remote" => {
            if !param.is_empty() {
                view_state
                    .pending_messages
                    .push(AppMessage::Fetch(Some(param.to_string())));
            }
        }
        // Staging actions
        "stage" => {
            if !param.is_empty() {
                view_state
                    .pending_messages
                    .push(AppMessage::StageFile(param.to_string()));
            }
        }
        "unstage" => {
            if !param.is_empty() {
                view_state
                    .pending_messages
                    .push(AppMessage::UnstageFile(param.to_string()));
            }
        }
        "view_diff" => {
            if !param.is_empty() {
                let staged = view_state
                    .staging_well
                    .staged_list
                    .files
                    .iter()
                    .any(|f| f.path == param);
                view_state
                    .pending_messages
                    .push(AppMessage::ViewDiff(param.to_string(), staged));
            }
        }
        "discard" => {
            if !param.is_empty() {
                confirm_dialog.show(
                    "Discard Changes",
                    &format!("Discard changes to '{}'? This cannot be undone.", param),
                );
                *pending_confirm_action = Some(AppMessage::DiscardFile(param.to_string()));
            }
        }
        "delete_submodule" => {
            if !param.is_empty() {
                confirm_dialog.show(
                    "Delete Submodule",
                    &format!(
                        "Remove submodule '{}'? This will deinit and remove it.",
                        param
                    ),
                );
                *pending_confirm_action = Some(AppMessage::DeleteSubmodule(param.to_string()));
            }
        }
        "update_submodule" => {
            if !param.is_empty() {
                view_state
                    .pending_messages
                    .push(AppMessage::UpdateSubmodule(param.to_string()));
            }
        }
        "enter_submodule" => {
            if !param.is_empty() {
                view_state
                    .pending_messages
                    .push(AppMessage::EnterSubmodule(param.to_string()));
            }
        }
        "open_submodule" => {
            if !param.is_empty() {
                let path = view_state
                    .staging_well
                    .submodules
                    .iter()
                    .find(|s| s.name == param)
                    .map(|s| s.path.clone());
                if let Some(path) = path {
                    open_terminal_at(&path, param, toast_manager);
                } else {
                    toast_manager.push(
                        format!("Submodule '{}' not found", param),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        "open_worktree" => {
            if !param.is_empty() {
                let path = view_state
                    .worktree_state
                    .worktrees
                    .iter()
                    .find(|w| w.name == param)
                    .map(|w| w.path.clone());
                if let Some(path) = path {
                    open_terminal_at(&path, param, toast_manager);
                } else {
                    toast_manager.push(
                        format!("Worktree '{}' not found", param),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        "switch_worktree" => {
            if !param.is_empty() {
                view_state.switch_to_worktree_by_name(param, repo);
            }
        }
        "jump_to_worktree" => {
            if !param.is_empty() {
                view_state
                    .pending_messages
                    .push(AppMessage::JumpToWorktreeBranch(param.to_string()));
            }
        }
        "remove_worktree" => {
            if !param.is_empty() {
                let is_dirty = view_state
                    .worktree_state
                    .worktrees
                    .iter()
                    .any(|w| w.name == param && w.is_dirty);
                let msg = if is_dirty {
                    format!(
                        "Remove worktree '{}'? This worktree has uncommitted changes that will be lost.",
                        param
                    )
                } else {
                    format!("Remove worktree '{}'?", param)
                };
                confirm_dialog.show("Remove Worktree", &msg);
                *pending_confirm_action = Some(AppMessage::RemoveWorktree(param.to_string()));
            }
        }
        "merge" => {
            if !param.is_empty() {
                let r = view_state.worktree_state.staging_repo_or(repo);
                let target_dir = view_state.worktree_state.selected_path.clone();
                if let Some(label) = crate::git::repo_state_label(r.repo_state()) {
                    toast_manager.push(
                        format!("Cannot merge: {}. Abort or complete it first.", label),
                        ToastSeverity::Error,
                    );
                } else {
                    let current = r.current_branch().unwrap_or_else(|_| "HEAD".to_string());
                    let uncommitted = r.uncommitted_change_count();
                    merge_dialog.show_with_target(param, &current, uncommitted, target_dir);
                }
            }
        }
        "rebase" => {
            if !param.is_empty() {
                let r = view_state.worktree_state.staging_repo_or(repo);
                let target_dir = view_state.worktree_state.selected_path.clone();
                if let Some(label) = crate::git::repo_state_label(r.repo_state()) {
                    toast_manager.push(
                        format!("Cannot rebase: {}. Abort or complete it first.", label),
                        ToastSeverity::Error,
                    );
                } else {
                    let current = r.current_branch().unwrap_or_else(|_| "HEAD".to_string());
                    let uncommitted = r.uncommitted_change_count();
                    rebase_dialog.show_with_target(param, &current, uncommitted, target_dir);
                }
            }
        }
        "cherry_pick" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                let target_dir = view_state.worktree_state.selected_path.clone();
                let branch = view_state.current_branch_opt().unwrap_or("HEAD");
                confirm_dialog.show(
                    "Cherry-pick",
                    &format!("Cherry-pick commit {} into '{}'?", short, branch),
                );
                *pending_confirm_action = Some(AppMessage::CherryPick(oid, target_dir));
            }
        }
        "revert_commit" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                let target_dir = view_state.worktree_state.selected_path.clone();
                let branch = view_state.current_branch_opt().unwrap_or("HEAD");
                confirm_dialog.show(
                    "Revert Commit",
                    &format!("Create a revert of {} on '{}'?", short, branch),
                );
                *pending_confirm_action = Some(AppMessage::RevertCommit(oid, target_dir));
            }
        }
        "reset_soft" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                let target_dir = view_state.worktree_state.selected_path.clone();
                let branch = view_state.current_branch_opt().unwrap_or("HEAD");
                confirm_dialog.show(
                    "Reset (Soft)",
                    &format!(
                        "Reset '{}' to {}? Changes will be kept staged.",
                        branch, short
                    ),
                );
                *pending_confirm_action = Some(AppMessage::ResetToCommit(
                    oid,
                    git2::ResetType::Soft,
                    target_dir,
                ));
            }
        }
        "reset_mixed" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                let target_dir = view_state.worktree_state.selected_path.clone();
                let branch = view_state.current_branch_opt().unwrap_or("HEAD");
                confirm_dialog.show(
                    "Reset (Mixed)",
                    &format!(
                        "Reset '{}' to {}? Changes will be kept unstaged.",
                        branch, short
                    ),
                );
                *pending_confirm_action = Some(AppMessage::ResetToCommit(
                    oid,
                    git2::ResetType::Mixed,
                    target_dir,
                ));
            }
        }
        "reset_hard" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                let target_dir = view_state.worktree_state.selected_path.clone();
                let branch = view_state.current_branch_opt().unwrap_or("HEAD");
                confirm_dialog.show("Reset (Hard)", &format!("Reset '{}' to {}?\n\nALL changes will be DISCARDED. This cannot be undone.", branch, short));
                *pending_confirm_action = Some(AppMessage::ResetToCommit(
                    oid,
                    git2::ResetType::Hard,
                    target_dir,
                ));
            }
        }
        "create_branch" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                let default_name = format!("branch-{}", short);
                branch_name_dialog.show(&default_name, oid);
            }
        }
        "create_worktree" => {
            if param.is_empty() {
                // From commit graph: use short SHA as source
                if let Some(oid) = view_state.context_menu_commit {
                    let short = &oid.to_string()[..7];
                    let default_name = format!("wt-{}", short);
                    branch_name_dialog.show_for_worktree(&default_name, short);
                }
            } else {
                // From branch sidebar: use branch name as source
                branch_name_dialog.show_for_worktree(param, param);
            }
        }
        "create_tag" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                let default_name = format!("v0.1.0-{}", short);
                branch_name_dialog.show_with_title("Create Tag", &default_name, oid);
            }
        }
        "delete_tag" => {
            if !param.is_empty() {
                confirm_dialog.show("Delete Tag", &format!("Delete tag '{}'?", param));
                *pending_confirm_action = Some(AppMessage::DeleteTag(param.to_string()));
            }
        }
        "stash_push" => {
            view_state.pending_messages.push(AppMessage::StashPush);
        }
        "stash_pop" => {
            view_state.pending_messages.push(AppMessage::StashPop);
        }
        "apply_stash" => {
            if let Ok(index) = param.parse::<usize>() {
                view_state
                    .pending_messages
                    .push(AppMessage::StashApply(index));
            }
        }
        "pop_stash" => {
            if let Ok(index) = param.parse::<usize>() {
                view_state
                    .pending_messages
                    .push(AppMessage::StashPopIndex(index));
            }
        }
        "drop_stash" => {
            if let Ok(index) = param.parse::<usize>() {
                confirm_dialog.show(
                    "Drop Stash",
                    &format!("Drop stash@{{{}}}? This cannot be undone.", index),
                );
                *pending_confirm_action = Some(AppMessage::StashDrop(index));
            }
        }
        "fetch_all" => {
            view_state.pending_messages.push(AppMessage::FetchAll);
        }
        "add_remote" => {
            remote_dialog.show_add();
        }
        "edit_remote_url" => {
            if !param.is_empty() {
                let current_url = repo.remote_url(param).unwrap_or_default();
                remote_dialog.show_edit_url(param, &current_url);
            }
        }
        "rename_remote" => {
            if !param.is_empty() {
                remote_dialog.show_rename(param);
            }
        }
        "delete_remote" => {
            if !param.is_empty() {
                confirm_dialog.show("Delete Remote", &format!("Delete remote '{}'? This will remove all remote-tracking branches for this remote.", param));
                *pending_confirm_action = Some(AppMessage::DeleteRemote(param.to_string()));
            }
        }
        "merge_remote" => {
            if !param.is_empty() {
                let r = view_state.worktree_state.staging_repo_or(repo);
                let target_dir = view_state.worktree_state.selected_path.clone();
                if let Some(label) = crate::git::repo_state_label(r.repo_state()) {
                    toast_manager.push(
                        format!("Cannot merge: {}. Abort or complete it first.", label),
                        ToastSeverity::Error,
                    );
                } else {
                    let current = r.current_branch().unwrap_or_else(|_| "HEAD".to_string());
                    let uncommitted = r.uncommitted_change_count();
                    merge_dialog.show_with_target(param, &current, uncommitted, target_dir);
                }
            }
        }
        "rebase_remote" => {
            if !param.is_empty() {
                let r = view_state.worktree_state.staging_repo_or(repo);
                let target_dir = view_state.worktree_state.selected_path.clone();
                if let Some(label) = crate::git::repo_state_label(r.repo_state()) {
                    toast_manager.push(
                        format!("Cannot rebase: {}. Abort or complete it first.", label),
                        ToastSeverity::Error,
                    );
                } else {
                    let current = r.current_branch().unwrap_or_else(|_| "HEAD".to_string());
                    let uncommitted = r.uncommitted_change_count();
                    rebase_dialog.show_with_target(param, &current, uncommitted, target_dir);
                }
            }
        }
        "delete_remote_branch" => {
            if !param.is_empty()
                && let Some((remote, branch)) = param.split_once('/')
            {
                confirm_dialog.show(
                    "Delete Remote Branch",
                    &format!(
                        "Delete branch '{}' from remote '{}'? This cannot be undone.",
                        branch, remote
                    ),
                );
                *pending_confirm_action = Some(AppMessage::DeleteRemoteBranch(
                    remote.to_string(),
                    branch.to_string(),
                ));
            }
        }
        "checkout_in_wt" => {
            // Format: "checkout_in_wt:branch|wt_name" — from context menu
            if !param.is_empty()
                && let Some((branch, wt_name)) = param.split_once('|')
            {
                if let Some(wt) = view_state
                    .worktree_state
                    .worktrees
                    .iter()
                    .find(|w| w.name == wt_name)
                {
                    view_state
                        .pending_messages
                        .push(AppMessage::CheckoutBranchInWorktree(
                            branch.to_string(),
                            PathBuf::from(&wt.path),
                        ));
                } else {
                    toast_manager.push(
                        format!("Worktree '{}' not found", wt_name),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        "set_head" => {
            if !param.is_empty() {
                view_state
                    .pending_messages
                    .push(AppMessage::SetHead(param.to_string()));
            }
        }
        _ => {
            toast_manager.push(
                format!("Unknown action: {}", action_id),
                ToastSeverity::Error,
            );
        }
    }

    view_state.context_menu_commit = None;
}

/// Add panel backgrounds, borders, and visual chrome to the output.
/// `mouse_pos` is used to highlight dividers on hover for drag affordance.
#[allow(clippy::too_many_arguments)]
fn add_panel_chrome(
    output: &mut WidgetOutput,
    layout: &ScreenLayout,
    screen_bounds: &Rect,
    focused: FocusedPanel,
    mouse_pos: (f32, f32),
    staging_mode: bool,
    staging_preview_ratio: f32,
    pill_bar_h: f32,
) {
    // Panel backgrounds for depth separation
    output
        .spline_vertices
        .extend(crate::ui::widget::create_rect_vertices(
            &layout.graph,
            theme::PANEL_GRAPH.to_array(),
        ));
    output
        .spline_vertices
        .extend(crate::ui::widget::create_rect_vertices(
            &layout.right_panel,
            theme::PANEL_STAGING.to_array(),
        ));

    // Border below shortcut bar (full width of screen)
    output
        .spline_vertices
        .extend(crate::ui::widget::create_rect_vertices(
            &Rect::new(0.0, layout.shortcut_bar.bottom(), screen_bounds.width, 1.0),
            theme::BORDER.to_array(),
        ));

    // Divider hover detection: brighten divider when mouse is within 8px (matches drag hit zone)
    let (mx, my) = mouse_pos;
    let hit_tolerance = 8.0;
    let in_content_area = my > layout.shortcut_bar.bottom();

    let sidebar_edge = layout.sidebar.right();
    let sidebar_graph_hover = in_content_area && (mx - sidebar_edge).abs() < hit_tolerance;

    let graph_edge = layout.graph.right();
    let graph_right_hover = in_content_area && (mx - graph_edge).abs() < hit_tolerance;

    // Vertical divider: sidebar | graph
    // Visible 2px line at rest, wider 3px highlighted line on hover
    if sidebar_graph_hover {
        output
            .spline_vertices
            .extend(crate::ui::widget::create_rect_vertices(
                &Rect::new(
                    layout.sidebar.right(),
                    layout.sidebar.y,
                    3.0,
                    layout.sidebar.height,
                ),
                theme::BORDER_LIGHT.to_array(),
            ));
    } else {
        output
            .spline_vertices
            .extend(crate::ui::widget::create_rect_vertices(
                &Rect::new(
                    layout.sidebar.right(),
                    layout.sidebar.y,
                    2.0,
                    layout.sidebar.height,
                ),
                theme::BORDER.with_alpha(0.50).to_array(),
            ));
    }

    // Vertical divider: graph | right panel
    if graph_right_hover {
        output
            .spline_vertices
            .extend(crate::ui::widget::create_rect_vertices(
                &Rect::new(
                    layout.graph.right(),
                    layout.graph.y,
                    3.0,
                    layout.graph.height,
                ),
                theme::BORDER_LIGHT.to_array(),
            ));
    } else {
        output
            .spline_vertices
            .extend(crate::ui::widget::create_rect_vertices(
                &Rect::new(
                    layout.graph.right(),
                    layout.graph.y,
                    2.0,
                    layout.graph.height,
                ),
                theme::BORDER.with_alpha(0.50).to_array(),
            ));
    }

    // Horizontal divider: staging | preview (within right panel, staging mode only)
    if staging_mode {
        let (_, content_rect) = layout.right_panel.take_top(pill_bar_h);
        let split_y = content_rect.y + content_rect.height * staging_preview_ratio;
        let hit_tolerance = 8.0;
        let staging_preview_hover =
            layout.right_panel.contains(mx, my) && (my - split_y).abs() < hit_tolerance;

        if staging_preview_hover {
            output
                .spline_vertices
                .extend(crate::ui::widget::create_rect_vertices(
                    &Rect::new(
                        layout.right_panel.x,
                        split_y - 1.0,
                        layout.right_panel.width,
                        3.0,
                    ),
                    theme::BORDER_LIGHT.to_array(),
                ));
        } else {
            output
                .spline_vertices
                .extend(crate::ui::widget::create_rect_vertices(
                    &Rect::new(layout.right_panel.x, split_y, layout.right_panel.width, 2.0),
                    theme::BORDER.with_alpha(0.50).to_array(),
                ));
        }
    }

    // Focused panel indicator: accent-colored top border (3px at ~60% alpha)
    let focused_rect = match focused {
        FocusedPanel::Graph => &layout.graph,
        FocusedPanel::RightPanel => &layout.right_panel,
        FocusedPanel::Sidebar => &layout.sidebar,
    };
    output
        .spline_vertices
        .extend(crate::ui::widget::create_rect_vertices(
            &Rect::new(focused_rect.x, focused_rect.y, focused_rect.width, 3.0),
            theme::ACCENT.with_alpha(0.6).to_array(),
        ));
}

/// Build the UI vertices for the active tab.
/// Takes separate borrows to avoid conflict between App fields and RenderState.
#[allow(clippy::too_many_arguments)]
fn build_ui_output(
    tabs: &mut [(RepoTab, TabViewState)],
    active_tab: usize,
    tab_bar: &TabBar,
    toast_manager: &mut ToastManager,
    tooltip: &mut Tooltip,
    repo_dialog: &RepoDialog,
    clone_dialog: &CloneDialog,
    settings_dialog: &SettingsDialog,
    confirm_dialog: &ConfirmDialog,
    branch_name_dialog: &BranchNameDialog,
    remote_dialog: &RemoteDialog,
    merge_dialog: &MergeDialog,
    rebase_dialog: &RebaseDialog,
    pull_dialog: &PullDialog,
    push_dialog: &PushDialog,
    text_renderer: &TextRenderer,
    bold_text_renderer: &TextRenderer,
    scale_factor: f64,
    extent: [u32; 2],
    avatar_cache: &mut AvatarCache,
    avatar_renderer: &AvatarRenderer,
    icon_renderer: &IconRenderer,
    sidebar_ratio: f32,
    graph_ratio: f32,
    staging_preview_ratio: f32,
    shortcut_bar_visible: bool,
    mouse_pos: (f32, f32),
    elapsed: f32,
) -> (WidgetOutput, WidgetOutput, WidgetOutput) {
    let screen_bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
    let scale = scale_factor as f32;

    // Tab bar takes space at top when multiple tabs
    let tab_bar_height = if tabs.len() > 1 {
        TabBar::height(scale)
    } else {
        0.0
    };
    let (tab_bar_bounds, main_bounds) = screen_bounds.take_top(tab_bar_height);
    let layout = ScreenLayout::compute_with_ratios_and_shortcut(
        main_bounds,
        4.0,
        scale,
        Some(sidebar_ratio),
        Some(graph_ratio),
        shortcut_bar_visible,
    );

    // Three layers: graph content renders first, chrome on top, overlay on top of everything
    let mut graph_output = WidgetOutput::new();
    let mut chrome_output = WidgetOutput::new();
    let mut overlay_output = WidgetOutput::new();

    // Panel backgrounds and borders go in graph layer (base - renders first, behind everything)
    let focused = tabs
        .get(active_tab)
        .map(|(_, vs)| vs.focused_panel)
        .unwrap_or_default();
    let staging_mode = tabs
        .get(active_tab)
        .map(|(_, vs)| vs.right_panel_mode == RightPanelMode::Staging)
        .unwrap_or(false);
    let pill_bar_h = tabs
        .get(active_tab)
        .map(|(_, vs)| vs.staging_well.pill_bar_height(&vs.current_branch))
        .unwrap_or(0.0);
    add_panel_chrome(
        &mut graph_output,
        &layout,
        &main_bounds,
        focused,
        mouse_pos,
        staging_mode,
        staging_preview_ratio,
        pill_bar_h,
    );

    // Active tab views
    if let Some((repo_tab, view_state)) = tabs.get_mut(active_tab) {
        // Commit graph (graph layer - renders first)
        let spline_vertices = view_state.commit_graph_view.layout_splines(
            text_renderer,
            &repo_tab.commits,
            layout.graph,
            view_state.head_oid,
        );
        let (text_vertices, pill_vertices, av_vertices) = view_state.commit_graph_view.layout_text(
            text_renderer,
            &repo_tab.commits,
            layout.graph,
            avatar_cache,
            avatar_renderer,
            view_state.head_oid,
            &view_state.worktree_state.worktrees,
        );
        graph_output.spline_vertices.extend(spline_vertices);
        graph_output.spline_vertices.extend(pill_vertices);
        graph_output.text_vertices.extend(text_vertices);
        graph_output.avatar_vertices.extend(av_vertices);

        // Offer tooltips for truncated commit subjects (uses current frame's data).
        // Suppress when any dialog or context menu is open.
        let any_modal_open = repo_dialog.is_visible()
            || clone_dialog.is_visible()
            || settings_dialog.is_visible()
            || confirm_dialog.is_visible()
            || branch_name_dialog.is_visible()
            || remote_dialog.is_visible()
            || merge_dialog.is_visible()
            || rebase_dialog.is_visible()
            || pull_dialog.is_visible()
            || push_dialog.is_visible()
            || view_state.context_menu.is_visible();
        tooltip.begin_frame();
        if !any_modal_open {
            let (mx, my) = mouse_pos;
            if layout.graph.contains(mx, my) {
                for (text_bounds, full_text) in &view_state.commit_graph_view.truncated_subjects {
                    if text_bounds.contains(mx, my) {
                        tooltip.offer(*text_bounds, full_text, mx, my);
                        break;
                    }
                }
                for (badge_bounds, hidden_labels) in
                    &view_state.commit_graph_view.overflow_pill_tooltips
                {
                    if badge_bounds.contains(mx, my) {
                        tooltip.offer(*badge_bounds, hidden_labels, mx, my);
                        break;
                    }
                }
            }
            // Header bar truncated buttons
            view_state
                .header_bar
                .report_tooltip(tooltip, mx, my, layout.header);
        }
        tooltip.end_frame();

        // Opaque header backdrop to prevent graph bleed-through between tab bar and header
        let header_backdrop_h = layout.header.height
            + if shortcut_bar_visible {
                layout.shortcut_bar.height
            } else {
                0.0
            };
        chrome_output
            .spline_vertices
            .extend(crate::ui::widget::create_rect_vertices(
                &Rect::new(
                    main_bounds.x,
                    main_bounds.y,
                    main_bounds.width,
                    header_backdrop_h,
                ),
                theme::SURFACE_RAISED.to_array(),
            ));

        // Header bar (chrome layer - on top of graph)
        chrome_output.extend(view_state.header_bar.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            layout.header,
            elapsed,
        ));

        // Shortcut bar (chrome layer - on top of graph) - only when visible
        if shortcut_bar_visible {
            chrome_output.extend(
                view_state
                    .shortcut_bar
                    .layout(text_renderer, layout.shortcut_bar),
            );
        }

        // Branch sidebar (chrome layer)
        chrome_output.extend(view_state.branch_sidebar.layout(
            text_renderer,
            bold_text_renderer,
            layout.sidebar,
            &view_state.current_branch,
            Some(icon_renderer),
        ));

        // Right panel (chrome layer) - worktree pills + mode-dependent content
        {
            let pill_bar_h = view_state
                .staging_well
                .pill_bar_height(&view_state.current_branch);
            let (pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);

            // Worktree pill bar (visible when there are worktree contexts)
            if pill_bar_h > 0.0 {
                chrome_output.extend(view_state.staging_well.layout_worktree_pills(
                    text_renderer,
                    pill_rect,
                    &view_state.current_branch,
                    &view_state.worktree_state.worktrees,
                ));
            }

            match view_state.right_panel_mode {
                RightPanelMode::Staging => {
                    // Upper: staging well, Lower: diff view with header
                    let (staging_rect, diff_rect) =
                        content_rect.split_vertical(staging_preview_ratio);
                    chrome_output
                        .extend(view_state.staging_well.layout(text_renderer, staging_rect));

                    let has_diff = view_state.diff_view.has_content();
                    let title = if has_diff {
                        view_state.diff_view.title()
                    } else {
                        "Preview"
                    };
                    let diff_body_rect = render_preview_header(
                        &mut chrome_output,
                        diff_rect,
                        title,
                        !has_diff,
                        scale,
                        bold_text_renderer,
                    );

                    if has_diff {
                        chrome_output
                            .extend(view_state.diff_view.layout(text_renderer, diff_body_rect));
                    } else {
                        let msg = "Select a file to preview its diff";
                        let msg_w = text_renderer.measure_text(msg);
                        let line_h = text_renderer.line_height();
                        let cx = diff_body_rect.x + (diff_body_rect.width - msg_w) / 2.0;
                        let cy = diff_body_rect.y + (diff_body_rect.height - line_h) / 2.0;
                        chrome_output
                            .text_vertices
                            .extend(text_renderer.layout_text(
                                msg,
                                cx,
                                cy,
                                theme::TEXT_MUTED.to_array(),
                            ));
                    }
                }
                RightPanelMode::Browse => {
                    // Upper: commit detail, Lower: diff view with header
                    if view_state.commit_detail_view.has_content() {
                        let (detail_rect, diff_rect) = content_rect.split_vertical(0.40);
                        chrome_output.extend(
                            view_state
                                .commit_detail_view
                                .layout(text_renderer, detail_rect),
                        );

                        let has_diff = view_state.diff_view.has_content();
                        let title = if has_diff {
                            view_state.diff_view.title()
                        } else {
                            "Diff"
                        };
                        let diff_body_rect = render_preview_header(
                            &mut chrome_output,
                            diff_rect,
                            title,
                            !has_diff,
                            scale,
                            bold_text_renderer,
                        );

                        if has_diff {
                            chrome_output
                                .extend(view_state.diff_view.layout(text_renderer, diff_body_rect));
                        }
                    } else if view_state.diff_view.has_content() {
                        let title = view_state.diff_view.title();
                        let diff_body_rect = render_preview_header(
                            &mut chrome_output,
                            content_rect,
                            title,
                            false,
                            scale,
                            bold_text_renderer,
                        );
                        chrome_output
                            .extend(view_state.diff_view.layout(text_renderer, diff_body_rect));
                    } else {
                        let body_rect = render_preview_header(
                            &mut chrome_output,
                            content_rect,
                            "Preview",
                            true,
                            scale,
                            bold_text_renderer,
                        );
                        let msg = "Select a commit to browse";
                        let msg_w = text_renderer.measure_text(msg);
                        let line_h = text_renderer.line_height();
                        let cx = body_rect.x + (body_rect.width - msg_w) / 2.0;
                        let cy = body_rect.y + (body_rect.height - line_h) / 2.0;
                        chrome_output
                            .text_vertices
                            .extend(text_renderer.layout_text(
                                msg,
                                cx,
                                cy,
                                theme::TEXT_MUTED.to_array(),
                            ));
                    }
                }
            }
        }
    }

    // Tab bar (chrome layer - rendered after graph so it draws on top)
    if tabs.len() > 1 {
        chrome_output.extend(tab_bar.layout(text_renderer, tab_bar_bounds));
    }

    // Context menu overlay (overlay layer - on top of all panels)
    if let Some((_, view_state)) = tabs.get_mut(active_tab)
        && view_state.context_menu.is_visible()
    {
        overlay_output.extend(view_state.context_menu.layout(text_renderer, screen_bounds));
    }

    // Toast notifications (overlay layer - on top of context menus)
    overlay_output.extend(toast_manager.layout(text_renderer, screen_bounds, scale));

    // Tooltip (overlay layer - on top of toasts, below dialogs)
    overlay_output.extend(tooltip.layout(text_renderer, screen_bounds, scale));

    // Repo dialog (overlay layer - on top of everything including toasts)
    if repo_dialog.is_visible() {
        overlay_output.extend(repo_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    // Clone dialog (overlay layer)
    if clone_dialog.is_visible() {
        overlay_output.extend(clone_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    // Settings dialog (overlay layer - on top of everything)
    if settings_dialog.is_visible() {
        overlay_output.extend(settings_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    // Confirm dialog (overlay layer - on top of everything including settings)
    if confirm_dialog.is_visible() {
        overlay_output.extend(confirm_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    // Branch name dialog (overlay layer - on top of everything)
    if branch_name_dialog.is_visible() {
        overlay_output.extend(branch_name_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    // Remote dialog (overlay layer - on top of everything)
    if remote_dialog.is_visible() {
        overlay_output.extend(remote_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    // Pull dialog (overlay layer - on top of everything)
    if pull_dialog.is_visible() {
        overlay_output.extend(pull_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    if push_dialog.is_visible() {
        overlay_output.extend(push_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    // Merge dialog (overlay layer - on top of everything)
    if merge_dialog.is_visible() {
        overlay_output.extend(merge_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    // Rebase dialog (overlay layer - on top of everything)
    if rebase_dialog.is_visible() {
        overlay_output.extend(rebase_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    (graph_output, chrome_output, overlay_output)
}

fn draw_frame(app: &mut App) -> Result<()> {
    let state = app.state.as_mut().unwrap();
    state
        .previous_frame_end
        .as_mut()
        .unwrap()
        .cleanup_finished();

    // Recreate swapchain if needed
    if state.surface.needs_recreate {
        state
            .surface
            .recreate(&state.ctx, state.window.inner_size())?;
    }

    // Acquire next image
    let (image_index, suboptimal, acquire_future) = match acquire_next_image(
        state.surface.swapchain.clone(),
        None,
    )
    .map_err(Validated::unwrap)
    {
        Ok(r) => r,
        Err(VulkanError::OutOfDate) => {
            state.surface.needs_recreate = true;
            return Ok(());
        }
        Err(e) => anyhow::bail!("Failed to acquire next image: {e:?}"),
    };

    if suboptimal {
        state.surface.needs_recreate = true;
    }

    // Sync button state and shortcut context before layout
    let single_tab = app.tabs.len() == 1;
    let now = Instant::now();
    let elapsed = app.app_start.elapsed().as_secs_f32();
    if let Some((_, view_state)) = app.tabs.get_mut(app.active_tab) {
        // Set generic op label from receiver (for spinner indicator in header)
        view_state.header_bar.generic_op_label =
            view_state
                .generic_op_receiver
                .as_ref()
                .map(|(_, label, _)| {
                    let dot_count = ((elapsed * 2.5) as usize % 3) + 1;
                    let dots: String = ".".repeat(dot_count);
                    format!("{}{}", label, dots)
                });
        view_state.header_bar.ci_status = view_state.ci_status.clone();
        let branch_opt = view_state.current_branch_opt().map(|s| s.to_string());
        view_state.header_bar.update_button_state(
            elapsed,
            branch_opt.as_deref(),
            &state.bold_text_renderer,
        );
        view_state.staging_well.update_button_state(elapsed);
        view_state.staging_well.update_cursors(now);
        view_state.commit_graph_view.search_bar.update_cursor(now);
        view_state.branch_sidebar.update_filter_cursor(now);
        view_state
            .shortcut_bar
            .set_context(match view_state.focused_panel {
                FocusedPanel::Graph => ShortcutContext::Graph,
                FocusedPanel::RightPanel => match view_state.right_panel_mode {
                    RightPanelMode::Staging => ShortcutContext::Staging,
                    RightPanelMode::Browse => ShortcutContext::Graph,
                },
                FocusedPanel::Sidebar => ShortcutContext::Sidebar,
            });
        view_state.shortcut_bar.show_new_tab_hint = single_tab;

        // Sync breadcrumb data from submodule focus state
        if let Some(ref focus) = view_state.submodule_focus {
            let home = std::env::var("HOME").unwrap_or_default();
            let mut segs: Vec<String> = Vec::new();
            for (i, s) in focus.parent_stack.iter().enumerate() {
                if i == 0 {
                    // First segment: show abbreviated repo path for the root repo
                    let root_path = s
                        .repo
                        .workdir()
                        .or_else(|| Some(s.repo.git_dir()))
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|| s.repo_name.clone());
                    let root_path = root_path.trim_end_matches('/').to_string();
                    if !home.is_empty() && root_path.starts_with(&home) {
                        segs.push(format!("~{}", &root_path[home.len()..]));
                    } else {
                        segs.push(root_path);
                    }
                } else {
                    // Intermediate segments: submodule names
                    segs.push(s.submodule_name.clone());
                }
            }
            segs.push(focus.current_name.clone());
            view_state.header_bar.breadcrumb_segments = segs;
        } else {
            view_state.header_bar.breadcrumb_segments.clear();
        }

        // Pre-compute breadcrumb segment bounds for hit testing
        // (needs approximate header bounds — compute from extent)
        let extent = state.surface.extent();
        let screen_bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
        let tab_bar_height = if single_tab {
            0.0
        } else {
            TabBar::height(state.scale_factor as f32)
        };
        let (_tb_bounds, main_bounds) = screen_bounds.take_top(tab_bar_height);
        let approx_layout = ScreenLayout::compute_with_ratios_and_shortcut(
            main_bounds,
            4.0,
            state.scale_factor as f32,
            Some(app.sidebar_ratio),
            Some(app.graph_ratio),
            app.shortcut_bar_visible,
        );
        view_state
            .header_bar
            .update_breadcrumb_bounds(&state.text_renderer, approx_layout.header);
        view_state
            .header_bar
            .update_abort_bounds(&state.text_renderer, approx_layout.header);
        view_state
            .header_bar
            .update_ci_bounds(&state.text_renderer, approx_layout.header);
    }

    // Update toast manager and tooltip
    app.toast_manager.update(Instant::now());
    app.tooltip.update();

    // Poll avatar downloads and pack newly loaded ones into the atlas
    let newly_loaded = state.avatar_cache.poll_downloads();
    for email in &newly_loaded {
        if let Some((rgba, size)) = state.avatar_cache.get_loaded(email) {
            state.avatar_renderer.pack_avatar(email, rgba, size);
        }
    }

    let extent = state.surface.extent();
    let scale_factor = state.scale_factor;
    let mouse_pos = state.input_state.mouse.position();
    let (sidebar_ratio, graph_ratio) = (app.sidebar_ratio, app.graph_ratio);
    let elapsed = app.app_start.elapsed().as_secs_f32();
    let (graph_output, chrome_output, overlay_output) = build_ui_output(
        &mut app.tabs,
        app.active_tab,
        &app.tab_bar,
        &mut app.toast_manager,
        &mut app.tooltip,
        &app.repo_dialog,
        &app.clone_dialog,
        &app.settings_dialog,
        &app.confirm_dialog,
        &app.branch_name_dialog,
        &app.remote_dialog,
        &app.merge_dialog,
        &app.rebase_dialog,
        &app.pull_dialog,
        &app.push_dialog,
        &state.text_renderer,
        &state.bold_text_renderer,
        scale_factor,
        extent,
        &mut state.avatar_cache,
        &state.avatar_renderer,
        &state.icon_renderer,
        sidebar_ratio,
        graph_ratio,
        app.staging_preview_ratio,
        app.shortcut_bar_visible,
        mouse_pos,
        elapsed,
    );

    let viewport = Viewport {
        offset: [0.0, 0.0],
        extent: [extent[0] as f32, extent[1] as f32],
        depth_range: 0.0..=1.0,
    };

    // Upload avatar/icon atlas in a separate command buffer if dirty
    if state.avatar_renderer.needs_upload() || state.icon_renderer.needs_upload() {
        let mut upload_builder = AutoCommandBufferBuilder::primary(
            state.ctx.command_buffer_allocator.clone(),
            state.ctx.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .context("Failed to create upload command buffer")?;

        state.avatar_renderer.upload_atlas(&mut upload_builder)?;
        state.icon_renderer.upload_atlas(&mut upload_builder)?;

        let upload_cb = upload_builder
            .build()
            .context("Failed to build upload command buffer")?;
        let upload_future = state
            .previous_frame_end
            .take()
            .unwrap()
            .then_execute(state.ctx.queue.clone(), upload_cb)
            .context("Failed to execute upload")?
            .then_signal_fence_and_flush()
            .map_err(Validated::unwrap)
            .context("Failed to flush upload")?;
        upload_future
            .wait(None)
            .context("Failed to wait for upload")?;
        state.previous_frame_end = Some(sync::now(state.ctx.device.clone()).boxed());
    }

    // Build command buffer
    let mut builder = AutoCommandBufferBuilder::primary(
        state.ctx.command_buffer_allocator.clone(),
        state.ctx.queue.queue_family_index(),
        CommandBufferUsage::OneTimeSubmit,
    )
    .context("Failed to create command buffer")?;

    builder
        .begin_render_pass(
            RenderPassBeginInfo {
                clear_values: vec![
                    Some(clear_color_for_format(state.surface.image_format()).into()),
                    None,
                ],
                ..RenderPassBeginInfo::framebuffer(
                    state.surface.framebuffers[image_index as usize].clone(),
                )
            },
            Default::default(),
        )
        .context("Failed to begin render pass")?;

    render_output_to_builder(&mut builder, state, graph_output, viewport.clone())?;
    render_output_to_builder(&mut builder, state, chrome_output, viewport.clone())?;
    render_output_to_builder(&mut builder, state, overlay_output, viewport)?;

    builder
        .end_render_pass(Default::default())
        .context("Failed to end render pass")?;

    let command_buffer = builder.build().context("Failed to build command buffer")?;

    // Submit
    let future = state
        .previous_frame_end
        .take()
        .unwrap()
        .join(acquire_future)
        .then_execute(state.ctx.queue.clone(), command_buffer)
        .context("Failed to execute")?
        .then_swapchain_present(
            state.ctx.queue.clone(),
            SwapchainPresentInfo::swapchain_image_index(
                state.surface.swapchain.clone(),
                image_index,
            ),
        )
        .then_signal_fence_and_flush();

    match future.map_err(Validated::unwrap) {
        Ok(future) => state.previous_frame_end = Some(future.boxed()),
        Err(VulkanError::OutOfDate) => {
            state.surface.needs_recreate = true;
            state.previous_frame_end = Some(sync::now(state.ctx.device.clone()).boxed());
        }
        Err(e) => {
            state.previous_frame_end = Some(sync::now(state.ctx.device.clone()).boxed());
            anyhow::bail!("Failed to flush: {e:?}");
        }
    }

    state.frame_count += 1;
    Ok(())
}

#[inline]
fn srgb_to_linear_channel(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn clear_color_for_format(format: Format) -> [f32; 4] {
    let bg = theme::BACKGROUND.to_array();
    if format.numeric_format_color() == Some(NumericFormat::SRGB) {
        [
            srgb_to_linear_channel(bg[0]),
            srgb_to_linear_channel(bg[1]),
            srgb_to_linear_channel(bg[2]),
            bg[3],
        ]
    } else {
        bg
    }
}

/// Draw the UI output into a command buffer builder (shared by all render paths).
fn render_output_to_builder(
    builder: &mut AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
    state: &RenderState,
    output: WidgetOutput,
    viewport: Viewport,
) -> Result<()> {
    if !output.spline_vertices.is_empty() {
        let spline_buffer = state
            .spline_renderer
            .create_vertex_buffer(output.spline_vertices)?;
        state
            .spline_renderer
            .draw(builder, spline_buffer, viewport.clone())?;
    }
    if !output.avatar_vertices.is_empty() {
        let avatar_buffer = state
            .avatar_renderer
            .create_vertex_buffer(output.avatar_vertices)?;
        state
            .avatar_renderer
            .draw(builder, avatar_buffer, viewport.clone())?;
    }
    if !output.icon_vertices.is_empty() {
        let icon_buffer = state
            .icon_renderer
            .create_vertex_buffer(output.icon_vertices)?;
        state
            .icon_renderer
            .draw(builder, icon_buffer, viewport.clone())?;
    }
    if !output.text_vertices.is_empty() {
        let vertex_buffer = state
            .text_renderer
            .create_vertex_buffer(output.text_vertices)?;
        state
            .text_renderer
            .draw(builder, vertex_buffer, viewport.clone())?;
    }
    if !output.bold_text_vertices.is_empty() {
        let bold_buffer = state
            .bold_text_renderer
            .create_vertex_buffer(output.bold_text_vertices)?;
        state
            .bold_text_renderer
            .draw(builder, bold_buffer, viewport)?;
    }
    Ok(())
}

fn capture_screenshot(app: &mut App) -> Result<image::RgbaImage> {
    let state = app.state.as_mut().unwrap();
    state
        .previous_frame_end
        .as_mut()
        .unwrap()
        .cleanup_finished();

    let extent = state.surface.extent();
    let scale_factor = state.scale_factor;
    let (sidebar_ratio, graph_ratio) = (app.sidebar_ratio, app.graph_ratio);
    let elapsed = app.app_start.elapsed().as_secs_f32();
    let (graph_output, chrome_output, overlay_output) = build_ui_output(
        &mut app.tabs,
        app.active_tab,
        &app.tab_bar,
        &mut app.toast_manager,
        &mut app.tooltip,
        &app.repo_dialog,
        &app.clone_dialog,
        &app.settings_dialog,
        &app.confirm_dialog,
        &app.branch_name_dialog,
        &app.remote_dialog,
        &app.merge_dialog,
        &app.rebase_dialog,
        &app.pull_dialog,
        &app.push_dialog,
        &state.text_renderer,
        &state.bold_text_renderer,
        scale_factor,
        extent,
        &mut state.avatar_cache,
        &state.avatar_renderer,
        &state.icon_renderer,
        sidebar_ratio,
        graph_ratio,
        app.staging_preview_ratio,
        app.shortcut_bar_visible,
        (0.0, 0.0), // No mouse interaction for screenshots
        elapsed,
    );

    let state = app.state.as_mut().unwrap();
    let viewport = Viewport {
        offset: [0.0, 0.0],
        extent: [extent[0] as f32, extent[1] as f32],
        depth_range: 0.0..=1.0,
    };

    // Upload avatar/icon atlas in a separate command buffer if dirty
    if state.avatar_renderer.needs_upload() || state.icon_renderer.needs_upload() {
        let mut upload_builder = AutoCommandBufferBuilder::primary(
            state.ctx.command_buffer_allocator.clone(),
            state.ctx.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .context("Failed to create upload command buffer")?;

        state.avatar_renderer.upload_atlas(&mut upload_builder)?;
        state.icon_renderer.upload_atlas(&mut upload_builder)?;

        let upload_cb = upload_builder
            .build()
            .context("Failed to build upload command buffer")?;
        let upload_future = state
            .previous_frame_end
            .take()
            .unwrap()
            .then_execute(state.ctx.queue.clone(), upload_cb)
            .context("Failed to execute upload")?
            .then_signal_fence_and_flush()
            .map_err(Validated::unwrap)
            .context("Failed to flush upload")?;
        upload_future
            .wait(None)
            .context("Failed to wait for upload")?;
        state.previous_frame_end = Some(sync::now(state.ctx.device.clone()).boxed());
    }

    // Acquire image
    let (image_index, _, acquire_future) =
        acquire_next_image(state.surface.swapchain.clone(), None)
            .map_err(Validated::unwrap)
            .context("Failed to acquire image")?;

    let mut builder = AutoCommandBufferBuilder::primary(
        state.ctx.command_buffer_allocator.clone(),
        state.ctx.queue.queue_family_index(),
        CommandBufferUsage::OneTimeSubmit,
    )
    .context("Failed to create command buffer")?;

    builder
        .begin_render_pass(
            RenderPassBeginInfo {
                clear_values: vec![
                    Some(clear_color_for_format(state.surface.image_format()).into()),
                    None,
                ],
                ..RenderPassBeginInfo::framebuffer(
                    state.surface.framebuffers[image_index as usize].clone(),
                )
            },
            Default::default(),
        )
        .context("Failed to begin render pass")?;

    render_output_to_builder(&mut builder, state, graph_output, viewport.clone())?;
    render_output_to_builder(&mut builder, state, chrome_output, viewport.clone())?;
    render_output_to_builder(&mut builder, state, overlay_output, viewport)?;

    builder
        .end_render_pass(Default::default())
        .context("Failed to end render pass")?;

    let capture = capture_to_buffer(
        &mut builder,
        state.ctx.memory_allocator.clone(),
        state.surface.images[image_index as usize].clone(),
        state.surface.image_format(),
    )?;

    let command_buffer = builder.build().context("Failed to build command buffer")?;

    let future = state
        .previous_frame_end
        .take()
        .unwrap()
        .join(acquire_future)
        .then_execute(state.ctx.queue.clone(), command_buffer)
        .context("Failed to execute")?
        .then_signal_fence_and_flush()
        .map_err(Validated::unwrap)
        .context("Failed to flush")?;

    future.wait(None).context("Failed to wait")?;
    state.previous_frame_end = Some(sync::now(state.ctx.device.clone()).boxed());

    capture.to_image()
}

fn capture_screenshot_offscreen(
    app: &mut App,
    width: u32,
    height: u32,
) -> Result<image::RgbaImage> {
    let state = app.state.as_mut().unwrap();
    state
        .previous_frame_end
        .as_mut()
        .unwrap()
        .cleanup_finished();

    let offscreen = OffscreenTarget::new(
        &state.ctx,
        state.surface.render_pass.clone(),
        width,
        height,
        state.surface.image_format(),
    )?;

    let extent = offscreen.extent();
    let scale_factor = state.scale_factor;
    let (sidebar_ratio, graph_ratio) = (app.sidebar_ratio, app.graph_ratio);
    let elapsed = app.app_start.elapsed().as_secs_f32();
    let (graph_output, chrome_output, overlay_output) = build_ui_output(
        &mut app.tabs,
        app.active_tab,
        &app.tab_bar,
        &mut app.toast_manager,
        &mut app.tooltip,
        &app.repo_dialog,
        &app.clone_dialog,
        &app.settings_dialog,
        &app.confirm_dialog,
        &app.branch_name_dialog,
        &app.remote_dialog,
        &app.merge_dialog,
        &app.rebase_dialog,
        &app.pull_dialog,
        &app.push_dialog,
        &state.text_renderer,
        &state.bold_text_renderer,
        scale_factor,
        extent,
        &mut state.avatar_cache,
        &state.avatar_renderer,
        &state.icon_renderer,
        sidebar_ratio,
        graph_ratio,
        app.staging_preview_ratio,
        app.shortcut_bar_visible,
        (0.0, 0.0), // No mouse interaction for offscreen screenshots
        elapsed,
    );

    let state = app.state.as_mut().unwrap();
    let viewport = Viewport {
        offset: [0.0, 0.0],
        extent: [extent[0] as f32, extent[1] as f32],
        depth_range: 0.0..=1.0,
    };

    // Upload avatar/icon atlas in a separate command buffer if dirty
    if state.avatar_renderer.needs_upload() || state.icon_renderer.needs_upload() {
        let mut upload_builder = AutoCommandBufferBuilder::primary(
            state.ctx.command_buffer_allocator.clone(),
            state.ctx.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .context("Failed to create upload command buffer")?;

        state.avatar_renderer.upload_atlas(&mut upload_builder)?;
        state.icon_renderer.upload_atlas(&mut upload_builder)?;

        let upload_cb = upload_builder
            .build()
            .context("Failed to build upload command buffer")?;
        let upload_future = state
            .previous_frame_end
            .take()
            .unwrap()
            .then_execute(state.ctx.queue.clone(), upload_cb)
            .context("Failed to execute upload")?
            .then_signal_fence_and_flush()
            .map_err(Validated::unwrap)
            .context("Failed to flush upload")?;
        upload_future
            .wait(None)
            .context("Failed to wait for upload")?;
        state.previous_frame_end = Some(sync::now(state.ctx.device.clone()).boxed());
    }

    let mut builder = AutoCommandBufferBuilder::primary(
        state.ctx.command_buffer_allocator.clone(),
        state.ctx.queue.queue_family_index(),
        CommandBufferUsage::OneTimeSubmit,
    )
    .context("Failed to create command buffer")?;

    builder
        .begin_render_pass(
            RenderPassBeginInfo {
                clear_values: vec![Some(clear_color_for_format(offscreen.format).into()), None],
                ..RenderPassBeginInfo::framebuffer(offscreen.framebuffer.clone())
            },
            Default::default(),
        )
        .context("Failed to begin render pass")?;

    render_output_to_builder(&mut builder, state, graph_output, viewport.clone())?;
    render_output_to_builder(&mut builder, state, chrome_output, viewport.clone())?;
    render_output_to_builder(&mut builder, state, overlay_output, viewport)?;

    builder
        .end_render_pass(Default::default())
        .context("Failed to end render pass")?;

    let capture = capture_to_buffer(
        &mut builder,
        state.ctx.memory_allocator.clone(),
        offscreen.image.clone(),
        offscreen.format,
    )?;

    let command_buffer = builder.build().context("Failed to build command buffer")?;

    let future = state
        .previous_frame_end
        .take()
        .unwrap()
        .then_execute(state.ctx.queue.clone(), command_buffer)
        .context("Failed to execute")?
        .then_signal_fence_and_flush()
        .map_err(Validated::unwrap)
        .context("Failed to flush")?;

    future.wait(None).context("Failed to wait")?;
    state.previous_frame_end = Some(sync::now(state.ctx.device.clone()).boxed());

    capture.to_image()
}

/// Apply a UI state for screenshot capture (e.g., showing dialogs, search bar, context menus).
fn apply_screenshot_state(app: &mut App) {
    let Some(ref state_str) = app.cli_args.screenshot_state else {
        return;
    };

    match state_str.as_str() {
        "open-dialog" => {
            app.repo_dialog.show();
        }
        "search" => {
            if let Some((_, view_state)) = app.tabs.get_mut(app.active_tab) {
                view_state.commit_graph_view.search_bar.activate();
                view_state.commit_graph_view.search_bar.set_query("example");
            }
        }
        "context-menu" => {
            let extent = app.state.as_ref().unwrap().surface.extent();
            let cx = extent[0] as f32 * 0.4;
            let cy = extent[1] as f32 * 0.3;
            if let Some((_, view_state)) = app.tabs.get_mut(app.active_tab) {
                let items = vec![
                    MenuItem::new("Copy SHA", "copy_sha"),
                    MenuItem::new("View Details", "view_details").with_shortcut("Enter"),
                    MenuItem::separator(),
                    MenuItem::new("Checkout", "checkout"),
                ];
                view_state.context_menu.show(items, cx, cy);
            }
        }
        "commit-detail" => {
            if let Some((repo_tab, view_state)) = app.tabs.get_mut(app.active_tab)
                && let Some(first) = repo_tab.commits.first()
            {
                let oid = first.id;
                let repo = &repo_tab.repo;
                if let Ok(info) = repo.full_commit_info(oid) {
                    let diff_files = repo.diff_for_commit(oid).unwrap_or_default();
                    let sm_entries = repo.submodules_at_commit(oid).unwrap_or_default();
                    view_state
                        .commit_detail_view
                        .set_commit(info, diff_files.clone(), sm_entries);
                    if let Some(first_file) = diff_files.first() {
                        let title = first_file.path.clone();
                        view_state
                            .diff_view
                            .set_diff(vec![first_file.clone()], title);
                    }
                }
            }
        }
        "confirm-dialog" => {
            app.confirm_dialog.show(
                "Delete Branch",
                "Delete branch 'feature'? This cannot be undone.",
            );
        }
        "merge-dialog" => {
            app.merge_dialog.show("feature", "main", 2);
        }
        "rebase-dialog" => {
            app.rebase_dialog.show("main", "feature", 1);
        }
        "pull-dialog" => {
            app.pull_dialog.show("main", "origin");
        }
        "push-dialog" => {
            app.push_dialog.show("main", "origin");
        }
        "settings-dialog" => {
            app.settings_dialog.show();
        }
        other => {
            eprintln!(
                "Unknown screenshot state: '{}'. Valid states: open-dialog, search, context-menu, commit-detail, confirm-dialog, merge-dialog, rebase-dialog, pull-dialog, push-dialog, settings-dialog",
                other
            );
        }
    }
}
