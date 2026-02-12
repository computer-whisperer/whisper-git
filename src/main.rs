mod config;
mod git;
mod input;
mod messages;
mod renderer;
mod ui;
mod views;
mod watcher;

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Instant;
use vulkano::{
    command_buffer::{AutoCommandBufferBuilder, CommandBufferUsage, PrimaryAutoCommandBuffer, RenderPassBeginInfo},
    pipeline::graphics::viewport::Viewport,
    swapchain::{acquire_next_image, SwapchainPresentInfo},
    sync::{self, GpuFuture},
    Validated, VulkanError,
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{CursorIcon, Window, WindowId},
};

use git2::Oid;

use crate::config::Config;
use crate::git::{CommitInfo, GitRepo, RemoteOpResult, SubmoduleInfo};
use crate::input::{InputEvent, InputState, Key};
use crate::renderer::{capture_to_buffer, OffscreenTarget, SurfaceManager, VulkanContext};
use crate::ui::{AvatarCache, AvatarRenderer, Rect, ScreenLayout, SplineRenderer, TextRenderer, Widget, WidgetOutput};
use crate::ui::widget::theme;
use crate::ui::widgets::{BranchNameDialog, BranchNameDialogAction, ConfirmDialog, ConfirmDialogAction, ContextMenu, MenuAction, MenuItem, HeaderBar, RemoteDialog, RemoteDialogAction, RepoDialog, RepoDialogAction, SettingsDialog, SettingsDialogAction, ShortcutBar, ShortcutContext, TabBar, TabAction, ToastManager, ToastSeverity};
use crate::messages::{AppMessage, MessageContext, MessageViewState, RightPanelMode, handle_app_message};
use crate::views::{BranchSidebar, CommitDetailView, CommitDetailAction, CommitGraphView, GraphAction, DiffView, DiffAction, StagingWell, StagingAction, SidebarAction};
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
                        && let (Ok(width), Ok(height)) = (w.parse(), h.parse()) {
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


/// Saved parent state when drilling into a submodule
struct SavedParentState {
    repo: GitRepo,
    commits: Vec<CommitInfo>,
    repo_name: String,
    graph_scroll_offset: f32,
    selected_commit: Option<Oid>,
    sidebar_scroll_offset: f32,
    submodule_name: String,
    parent_submodules: Vec<SubmoduleInfo>,
}

/// Focus state when viewing a submodule (supports nesting via stack)
struct SubmoduleFocus {
    parent_stack: Vec<SavedParentState>,
    current_name: String,
}

/// Per-tab repository data
struct RepoTab {
    repo: Option<GitRepo>,
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
    fetch_receiver: Option<(Receiver<RemoteOpResult>, Instant)>,
    pull_receiver: Option<(Receiver<RemoteOpResult>, Instant)>,
    push_receiver: Option<(Receiver<RemoteOpResult>, Instant)>,
    /// Generic async receiver for submodule/worktree ops (label for toast)
    generic_op_receiver: Option<(Receiver<RemoteOpResult>, String, Instant)>,
    /// Track whether we already showed the "still running" toast for each op
    showed_timeout_toast: [bool; 4],
    /// Worktree info list (moved from sidebar)
    worktrees: Vec<crate::git::WorktreeInfo>,
    /// Cache of opened worktree repos keyed by path, to avoid re-discovering on switch
    worktree_repo_cache: HashMap<PathBuf, GitRepo>,
    /// Path of the currently active worktree (None = main worktree)
    active_worktree_path: Option<PathBuf>,
    /// Submodule drill-down state (None when viewing root repo)
    submodule_focus: Option<SubmoduleFocus>,
    /// Filesystem watcher for auto-refresh on external changes
    watcher: Option<RepoWatcher>,
    watcher_rx: Option<Receiver<FsChangeKind>>,
}

impl TabViewState {
    /// Switch the staging well to a different worktree by index.
    /// Dismisses any commit inspect activity and enters staging mode.
    fn switch_to_worktree(&mut self, index: usize) {
        self.staging_well.switch_worktree(index);
        self.right_panel_mode = RightPanelMode::Staging;
        self.diff_view.clear();
        self.commit_detail_view.clear();
        self.commit_graph_view.selected_commit = None;
        self.last_diff_commit = None;
        if let Some(wt_ctx) = self.staging_well.active_worktree_context() {
            if wt_ctx.is_current {
                self.active_worktree_path = None;
            } else {
                let path = wt_ctx.path.clone();
                if !self.worktree_repo_cache.contains_key(&path) {
                    if let Ok(repo) = GitRepo::open(&path) {
                        self.worktree_repo_cache.insert(path.clone(), repo);
                    }
                }
                self.active_worktree_path = Some(path);
            }
        }
    }

    /// Get a reference to the active worktree repo, if any.
    fn active_worktree_repo(&self) -> Option<&GitRepo> {
        self.active_worktree_path.as_ref()
            .and_then(|path| self.worktree_repo_cache.get(path))
    }

    /// Switch to a named worktree (looks up index by name).
    fn switch_to_worktree_by_name(&mut self, name: &str) {
        if let Some(idx) = self.staging_well.worktree_index_by_name(name) {
            self.switch_to_worktree(idx);
        }
    }

    /// Handle a staging action by dispatching to the appropriate pending message.
    fn handle_staging_action(&mut self, action: StagingAction) {
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
                let staged = self.staging_well.staged_list.files
                    .iter().any(|f| f.path == path);
                self.pending_messages.push(AppMessage::ViewDiff(path, staged));
            }
            StagingAction::SwitchWorktree(index) => {
                self.switch_to_worktree(index);
            }
            StagingAction::PreviewDiff(path, staged) => {
                self.pending_messages.push(AppMessage::ViewDiff(path, staged));
            }
            StagingAction::OpenSubmodule(name) => {
                self.pending_messages.push(AppMessage::EnterSubmodule(name));
            }
        }
    }

    /// Handle a graph action by dispatching to the appropriate pending message.
    fn handle_graph_action(&mut self, action: GraphAction) {
        match action {
            GraphAction::LoadMore => {
                self.pending_messages.push(AppMessage::LoadMoreCommits);
            }
            GraphAction::SwitchWorktree(name) => {
                self.switch_to_worktree_by_name(&name);
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
            worktrees: Vec::new(),
            worktree_repo_cache: HashMap::new(),
            active_worktree_path: None,
            submodule_focus: None,
            watcher: None,
            watcher_rx: None,
        }
    }
}

// ============================================================================
// Application
// ============================================================================

fn main() -> Result<()> {
    let cli_args = parse_args();

    let event_loop = EventLoop::new().context("Failed to create event loop")?;
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::new(cli_args)?;

    event_loop.run_app(&mut app).context("Event loop error")?;

    Ok(())
}

/// Which divider is currently being dragged
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DividerDrag {
    /// Vertical divider between sidebar and graph
    SidebarGraph,
    /// Vertical divider between graph and right panel
    GraphRight,
}

struct App {
    cli_args: CliArgs,
    config: Config,
    tabs: Vec<(RepoTab, TabViewState)>,
    active_tab: usize,
    tab_bar: TabBar,
    repo_dialog: RepoDialog,
    settings_dialog: SettingsDialog,
    confirm_dialog: ConfirmDialog,
    branch_name_dialog: BranchNameDialog,
    remote_dialog: RemoteDialog,
    pending_confirm_action: Option<AppMessage>,
    toast_manager: ToastManager,
    state: Option<RenderState>,
    /// Which divider is currently being dragged, if any
    divider_drag: Option<DividerDrag>,
    /// Fraction of total width for sidebar (default ~0.14)
    sidebar_ratio: f32,
    /// Fraction of content width (after sidebar) for graph (default 0.55)
    graph_ratio: f32,
    /// Whether the shortcut bar is visible
    shortcut_bar_visible: bool,
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
    previous_frame_end: Option<Box<dyn GpuFuture>>,
    frame_count: u32,
    scale_factor: f64,
    input_state: InputState,
}

impl App {
    fn new(cli_args: CliArgs) -> Result<Self> {
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
                            repo: Some(repo),
                            commits: Vec::new(),
                            name,
                        },
                        TabViewState::new(),
                    ));
                }
                Err(e) => {
                    eprintln!("Warning: Could not open repository at {:?}: {e}", repo_path);
                    // Still add a tab with no repo
                    let name = repo_path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    tab_bar.add_tab(name.clone());
                    tabs.push((
                        RepoTab {
                            repo: None,
                            commits: Vec::new(),
                            name,
                        },
                        TabViewState::new(),
                    ));
                }
            }
        }

        let mut settings_dialog = SettingsDialog::new();
        settings_dialog.show_avatars = config.avatars_enabled;
        settings_dialog.scroll_speed = if config.fast_scroll { 2.0 } else { 1.0 };
        settings_dialog.row_scale = config.row_scale;
        settings_dialog.abbreviate_worktree_names = config.abbreviate_worktree_names;
        settings_dialog.time_spacing_strength = config.time_spacing_strength;
        let shortcut_bar_visible = config.shortcut_bar_visible;

        Ok(Self {
            cli_args,
            config,
            tabs,
            active_tab: 0,
            tab_bar,
            repo_dialog: RepoDialog::new(),
            settings_dialog,
            confirm_dialog: ConfirmDialog::new(),
            branch_name_dialog: BranchNameDialog::new(),
            remote_dialog: RemoteDialog::new(),
            pending_confirm_action: None,
            toast_manager: ToastManager::new(),
            state: None,
            divider_drag: None,
            sidebar_ratio: 0.14,
            graph_ratio: 0.55,
            shortcut_bar_visible,
            current_cursor: CursorIcon::Default,
            status_dirty: true,
            last_status_refresh: Instant::now(),
            diff_stats_receiver: None,
            app_start: Instant::now(),
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

        // Create render pass with MSAA 4x
        let image_format = ctx.device.physical_device()
            .surface_formats(&surface, Default::default())
            .unwrap()[0].0;
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

        // Build font atlas at the max scale across all monitors for crisp text everywhere.
        // CLI --scale overrides for deterministic screenshots.
        let window_scale = window.scale_factor();
        let max_scale = self.cli_args.screenshot_scale.unwrap_or_else(|| {
            window.available_monitors()
                .map(|m| m.scale_factor())
                .fold(window_scale, f64::max)
        });
        let text_renderer = TextRenderer::new(
            ctx.memory_allocator.clone(),
            render_pass.clone(),
            &mut upload_builder,
            max_scale,
        )
        .context("Failed to create text renderer")?;

        let bold_text_renderer = TextRenderer::new_bold(
            ctx.memory_allocator.clone(),
            render_pass.clone(),
            &mut upload_builder,
            max_scale,
        )
        .context("Failed to create bold text renderer")?;

        let spline_renderer = SplineRenderer::new(
            ctx.memory_allocator.clone(),
            render_pass.clone(),
        )
        .context("Failed to create spline renderer")?;

        let avatar_renderer = AvatarRenderer::new(
            ctx.memory_allocator.clone(),
            render_pass.clone(),
        )
        .context("Failed to create avatar renderer")?;

        // Submit font atlas upload
        let upload_buffer = upload_builder.build().context("Failed to build upload buffer")?;
        let upload_future = sync::now(ctx.device.clone())
            .then_execute(ctx.queue.clone(), upload_buffer)
            .context("Failed to execute upload")?
            .then_signal_fence_and_flush()
            .map_err(Validated::unwrap)
            .context("Failed to flush upload")?;
        upload_future.wait(None).context("Failed to wait for upload")?;

        let previous_frame_end = Some(sync::now(ctx.device.clone()).boxed());

        // Initialize all tab views
        let scale = window_scale as f32;
        let row_scale = self.settings_dialog.row_scale;
        let abbreviate_wt = self.settings_dialog.abbreviate_worktree_names;
        let time_strength = self.settings_dialog.time_spacing_strength;
        for (repo_tab, view_state) in &mut self.tabs {
            view_state.commit_graph_view.row_scale = row_scale;
            view_state.commit_graph_view.abbreviate_worktree_names = abbreviate_wt;
            view_state.commit_graph_view.time_spacing_strength = time_strength;
            let rx = init_tab_view(repo_tab, view_state, &text_renderer, scale, &mut self.toast_manager);
            if rx.is_some() { self.diff_stats_receiver = rx; }
        }

        self.state = Some(RenderState {
            window,
            ctx,
            surface: surface_mgr,
            text_renderer,
            bold_text_renderer,
            spline_renderer,
            avatar_renderer,
            avatar_cache: AvatarCache::new(),
            previous_frame_end,
            frame_count: 0,
            scale_factor: window_scale,
            input_state: InputState::new(),
        });

        // Initial status refresh for active tab
        self.refresh_status();

        Ok(())
    }

    fn refresh_status(&mut self) {
        if let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) {
            let Some(ref repo) = repo_tab.repo else { return };

            // Active worktree status for staging well
            let staging_repo = view_state.active_worktree_repo().unwrap_or(repo);
            if let Ok(status) = staging_repo.status() {
                view_state.staging_well.update_status(&status);
            }

            // Main worktree status for graph + header (always from main repo)
            if let Ok(status) = repo.status() {
                view_state.commit_graph_view.working_dir_status = Some(status.clone());
                view_state.header_bar.has_staged = !status.staged.is_empty();
            }

            // Ahead/behind always from main repo
            if let Ok((ahead, behind)) = repo.ahead_behind() {
                view_state.header_bar.ahead = ahead;
                view_state.header_bar.behind = behind;
            }
        }
    }

    fn process_messages(&mut self) {
        let tab_count = self.tabs.len();
        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else { return };

        // Extract messages to avoid borrow conflicts
        let messages: Vec<_> = view_state.pending_messages.drain(..).collect();

        if messages.is_empty() {
            return;
        }

        // Partition: submodule navigation vs normal messages
        let (nav_messages, normal_messages): (Vec<_>, Vec<_>) = messages.into_iter().partition(|msg| {
            matches!(msg, AppMessage::EnterSubmodule(_) | AppMessage::ExitSubmodule | AppMessage::ExitToDepth(_))
        });

        // Handle submodule navigation first (needs text_renderer from self.state)
        if !nav_messages.is_empty() {
            if let Some(ref state) = self.state {
                let scale = state.scale_factor as f32;
                for msg in nav_messages {
                    match msg {
                        AppMessage::EnterSubmodule(name) => {
                            enter_submodule(&name, repo_tab, view_state, &state.text_renderer, scale, &mut self.toast_manager);
                        }
                        AppMessage::ExitSubmodule => {
                            exit_submodule(repo_tab, view_state, &state.text_renderer, scale, &mut self.toast_manager);
                        }
                        AppMessage::ExitToDepth(depth) => {
                            exit_to_depth(depth, repo_tab, view_state, &state.text_renderer, scale, &mut self.toast_manager);
                        }
                        _ => unreachable!(),
                    }
                }
            }
        }

        if normal_messages.is_empty() {
            return;
        }

        let scale = self.state.as_ref().map(|s| s.scale_factor as f32).unwrap_or(1.0);

        // Compute graph bounds for JumpToWorktreeBranch
        let graph_bounds = if let Some(ref state) = self.state {
            let extent = state.surface.extent();
            let screen_bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
            let tab_bar_height = if tab_count > 1 { TabBar::height(scale) } else { 0.0 };
            let (_tab_bar_bounds, main_bounds) = screen_bounds.take_top(tab_bar_height);
            let layout = ScreenLayout::compute_with_ratios_and_shortcut(
                main_bounds, 4.0, scale,
                Some(self.sidebar_ratio),
                Some(self.graph_ratio),
                self.shortcut_bar_visible,
            );
            layout.graph
        } else {
            Rect::new(0.0, 0.0, 1920.0, 1080.0)
        };

        let ctx = MessageContext { graph_bounds };

        let Some(ref repo) = repo_tab.repo else {
            return;
        };

        // Any normal message likely changes state, so mark status dirty
        self.status_dirty = true;

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
                worktrees: &mut view_state.worktrees,
            };
            let staging_repo = view_state.active_worktree_path.as_ref()
                .and_then(|p| view_state.worktree_repo_cache.get(p))
                .unwrap_or(repo);
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
        let Some((repo_tab, _view_state)) = self.tabs.get_mut(self.active_tab) else { return };
        let Some(ref repo) = repo_tab.repo else { return };
        let needs_stats: Vec<Oid> = repo_tab.commits.iter()
            .filter(|c| !c.is_synthetic && c.insertions == 0 && c.deletions == 0)
            .map(|c| c.id)
            .collect();
        if !needs_stats.is_empty() {
            self.diff_stats_receiver = Some(repo.compute_diff_stats_async(needs_stats));
        }
    }

    fn poll_watcher(&mut self) {
        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else { return };
        let Some(ref rx) = view_state.watcher_rx else { return };

        // Drain all pending signals, track the highest-priority kind
        let mut max_kind: Option<FsChangeKind> = None;
        while let Ok(kind) = rx.try_recv() {
            max_kind = Some(match max_kind {
                Some(prev) => if kind.priority() > prev.priority() { kind } else { prev },
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
                let rx = refresh_repo_state(repo_tab, view_state, &mut self.toast_manager);
                if rx.is_some() { self.diff_stats_receiver = rx; }
            }
            Some(FsChangeKind::WorktreeStructure) => {
                self.status_dirty = true;
                let rx = refresh_repo_state(repo_tab, view_state, &mut self.toast_manager);
                if rx.is_some() { self.diff_stats_receiver = rx; }
                // Update watcher paths for new/removed worktrees
                if let Some(ref repo) = repo_tab.repo {
                    let git_dir = repo.git_dir().to_path_buf();
                    if let Some(ref mut w) = view_state.watcher {
                        w.update_worktree_watches(&view_state.worktrees, &git_dir);
                    }
                }
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

        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else { return };
        let now = Instant::now();
        const TIMEOUT_SECS: u64 = 60;

        // Poll fetch
        if let Some((ref rx, started)) = view_state.fetch_receiver {
            match rx.try_recv() {
                Ok(result) => {
                    view_state.header_bar.fetching = false;
                    view_state.fetch_receiver = None;
                    view_state.showed_timeout_toast[0] = false;
                    if result.success {
                        self.toast_manager.push("Fetch complete", ToastSeverity::Success);
                        let rx = refresh_repo_state(repo_tab, view_state, &mut self.toast_manager);
                        if rx.is_some() { self.diff_stats_receiver = rx; }
                    } else {
                        let (msg, _) = classify_git_error("Fetch", &result.error);
                        self.toast_manager.push(msg, ToastSeverity::Error);
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    view_state.header_bar.fetching = false;
                    view_state.fetch_receiver = None;
                    view_state.showed_timeout_toast[0] = false;
                    self.toast_manager.push("Fetch failed: background thread terminated", ToastSeverity::Error);
                }
                Err(TryRecvError::Empty) => {
                    if now.duration_since(started).as_secs() >= TIMEOUT_SECS && !view_state.showed_timeout_toast[0] {
                        view_state.showed_timeout_toast[0] = true;
                        self.toast_manager.push("Fetch still running...", ToastSeverity::Info);
                    }
                }
            }
        }

        // Poll pull
        if let Some((ref rx, started)) = view_state.pull_receiver {
            match rx.try_recv() {
                Ok(result) => {
                    view_state.header_bar.pulling = false;
                    view_state.pull_receiver = None;
                    view_state.showed_timeout_toast[1] = false;
                    if result.success {
                        self.toast_manager.push("Pull complete", ToastSeverity::Success);
                        let rx = refresh_repo_state(repo_tab, view_state, &mut self.toast_manager);
                        if rx.is_some() { self.diff_stats_receiver = rx; }
                    } else {
                        let (msg, _) = classify_git_error("Pull", &result.error);
                        self.toast_manager.push(msg, ToastSeverity::Error);
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    view_state.header_bar.pulling = false;
                    view_state.pull_receiver = None;
                    view_state.showed_timeout_toast[1] = false;
                    self.toast_manager.push("Pull failed: background thread terminated", ToastSeverity::Error);
                }
                Err(TryRecvError::Empty) => {
                    if now.duration_since(started).as_secs() >= TIMEOUT_SECS && !view_state.showed_timeout_toast[1] {
                        view_state.showed_timeout_toast[1] = true;
                        self.toast_manager.push("Pull still running...", ToastSeverity::Info);
                    }
                }
            }
        }

        // Poll push (also does full refresh to update ahead/behind and branch state)
        if let Some((ref rx, started)) = view_state.push_receiver {
            match rx.try_recv() {
                Ok(result) => {
                    view_state.header_bar.pushing = false;
                    view_state.push_receiver = None;
                    view_state.showed_timeout_toast[2] = false;
                    if result.success {
                        self.toast_manager.push("Push complete", ToastSeverity::Success);
                        let rx = refresh_repo_state(repo_tab, view_state, &mut self.toast_manager);
                        if rx.is_some() { self.diff_stats_receiver = rx; }
                    } else {
                        let (msg, _) = classify_git_error("Push", &result.error);
                        self.toast_manager.push(msg, ToastSeverity::Error);
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    view_state.header_bar.pushing = false;
                    view_state.push_receiver = None;
                    view_state.showed_timeout_toast[2] = false;
                    self.toast_manager.push("Push failed: background thread terminated", ToastSeverity::Error);
                }
                Err(TryRecvError::Empty) => {
                    if now.duration_since(started).as_secs() >= TIMEOUT_SECS && !view_state.showed_timeout_toast[2] {
                        view_state.showed_timeout_toast[2] = true;
                        self.toast_manager.push("Push still running...", ToastSeverity::Info);
                    }
                }
            }
        }

        // Poll generic async ops (submodule/worktree operations)
        if let Some((ref rx, ref label, started)) = view_state.generic_op_receiver {
            let label = label.clone();
            match rx.try_recv() {
                Ok(result) => {
                    view_state.generic_op_receiver = None;
                    view_state.showed_timeout_toast[3] = false;
                    if result.success {
                        self.toast_manager.push(format!("{} complete", label), ToastSeverity::Success);
                        let rx = refresh_repo_state(repo_tab, view_state, &mut self.toast_manager);
                        if rx.is_some() { self.diff_stats_receiver = rx; }
                        // Also refresh worktrees/stashes
                        if let Some(ref repo) = repo_tab.repo {
                            if let Ok(worktrees) = repo.worktrees() {
                                view_state.worktrees = worktrees;
                            }
                            view_state.branch_sidebar.stashes = repo.stash_list();
                        }
                    } else {
                        let (msg, _) = classify_git_error(&label, &result.error);
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
                    if now.duration_since(started).as_secs() >= TIMEOUT_SECS && !view_state.showed_timeout_toast[3] {
                        view_state.showed_timeout_toast[3] = true;
                        self.toast_manager.push(format!("{} still running...", label), ToastSeverity::Info);
                    }
                }
            }
        }
    }

    /// Open a new repo and add it as a tab
    fn open_repo_tab(&mut self, path: PathBuf) {
        match GitRepo::open(&path) {
            Ok(repo) => {
                let name = repo.repo_name();
                self.tab_bar.add_tab(name.clone());
                let mut view_state = TabViewState::new();

                // Initialize the view if render state exists
                // (init_tab_view -> refresh_repo_state will load commits)
                let mut repo_tab = RepoTab {
                    repo: Some(repo),
                    commits: Vec::new(),
                    name,
                };

                if let Some(ref render_state) = self.state {
                    view_state.commit_graph_view.row_scale = self.settings_dialog.row_scale;
                    view_state.commit_graph_view.abbreviate_worktree_names = self.settings_dialog.abbreviate_worktree_names;
                    view_state.commit_graph_view.time_spacing_strength = self.settings_dialog.time_spacing_strength;
                    let rx = init_tab_view(&mut repo_tab, &mut view_state, &render_state.text_renderer, render_state.scale_factor as f32, &mut self.toast_manager);
                    if rx.is_some() { self.diff_stats_receiver = rx; }
                }

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
                self.toast_manager.push(
                    format!("Failed to open: {}", e),
                    ToastSeverity::Error,
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
        self.toast_manager.push(
            format!("Closed {}", name),
            ToastSeverity::Info,
        );
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
                        if let Some(msg) = self.pending_confirm_action.take() {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                view_state.pending_messages.push(msg);
                            }
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
            self.branch_name_dialog.handle_event(input_event, screen_bounds);
            if let Some(action) = self.branch_name_dialog.take_action() {
                match action {
                    BranchNameDialogAction::Create(name, oid) => {
                        if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                            if is_tag {
                                view_state.pending_messages.push(AppMessage::CreateTag(name, oid));
                            } else {
                                view_state.pending_messages.push(AppMessage::CreateBranch(name, oid));
                            }
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
                            view_state.pending_messages.push(AppMessage::AddRemote(name, url));
                        }
                    }
                    RemoteDialogAction::EditUrl(name, url) => {
                        if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                            view_state.pending_messages.push(AppMessage::SetRemoteUrl(name, url));
                        }
                    }
                    RemoteDialogAction::Rename(old_name, new_name) => {
                        if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                            view_state.pending_messages.push(AppMessage::RenameRemote(old_name, new_name));
                        }
                    }
                    RemoteDialogAction::Cancel => {}
                }
            }
            return true;
        }

        // Settings dialog takes priority (modal)
        if self.settings_dialog.is_visible() {
            self.settings_dialog.handle_event(input_event, screen_bounds);
            if let Some(action) = self.settings_dialog.take_action() {
                match action {
                    SettingsDialogAction::Close => {
                        let row_scale = self.settings_dialog.row_scale;
                        let abbreviate_wt = self.settings_dialog.abbreviate_worktree_names;
                        let time_strength = self.settings_dialog.time_spacing_strength;
                        if let Some(ref state) = self.state {
                            for (repo_tab, view_state) in &mut self.tabs {
                                view_state.commit_graph_view.row_scale = row_scale;
                                view_state.commit_graph_view.abbreviate_worktree_names = abbreviate_wt;
                                view_state.commit_graph_view.time_spacing_strength = time_strength;
                                view_state.commit_graph_view.sync_metrics(&state.text_renderer);
                                view_state.commit_graph_view.compute_row_offsets(&repo_tab.commits);
                            }
                        }
                        self.config.avatars_enabled = self.settings_dialog.show_avatars;
                        self.config.fast_scroll = self.settings_dialog.scroll_speed >= 1.5;
                        self.config.row_scale = self.settings_dialog.row_scale;
                        self.config.abbreviate_worktree_names = self.settings_dialog.abbreviate_worktree_names;
                        self.config.time_spacing_strength = self.settings_dialog.time_spacing_strength;
                        self.config.save();
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

        // Toast click-to-dismiss (overlay, before context menu)
        if self.toast_manager.handle_event(input_event, screen_bounds) {
            return true;
        }

        // Context menu takes priority when visible (overlay)
        if let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab)
            && view_state.context_menu.is_visible() {
                view_state.context_menu.handle_event(input_event, screen_bounds);
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
                                repo_tab.repo.as_ref(),
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
    fn handle_divider_drag(&mut self, input_event: &InputEvent, main_bounds: Rect, layout: &ScreenLayout) -> bool {
        // Handle ongoing drag (MouseMove / MouseUp) before anything else
        if self.divider_drag.is_some() {
            match input_event {
                InputEvent::MouseMove { x, .. } => {
                    match self.divider_drag.unwrap() {
                        DividerDrag::SidebarGraph => {
                            let ratio = (*x - main_bounds.x) / main_bounds.width;
                            self.sidebar_ratio = ratio.clamp(0.05, 0.30);
                        }
                        DividerDrag::GraphRight => {
                            let sidebar_w = main_bounds.width * self.sidebar_ratio.clamp(0.05, 0.30);
                            let content_x = main_bounds.x + sidebar_w;
                            let content_w = main_bounds.width - sidebar_w;
                            if content_w > 0.0 {
                                let ratio = (*x - content_x) / content_w;
                                self.graph_ratio = ratio.clamp(0.30, 0.80);
                            }
                        }
                    }
                    if let Some(ref render_state) = self.state {
                        let cursor = CursorIcon::ColResize;
                        if self.current_cursor != cursor {
                            render_state.window.set_cursor(cursor);
                            self.current_cursor = cursor;
                        }
                    }
                    return true;
                }
                InputEvent::MouseUp { .. } => {
                    self.divider_drag = None;
                    if let Some(ref render_state) = self.state {
                        if self.current_cursor != CursorIcon::Default {
                            render_state.window.set_cursor(CursorIcon::Default);
                            self.current_cursor = CursorIcon::Default;
                        }
                    }
                    return true;
                }
                _ => {}
            }
        }

        // Start divider drag on MouseDown near divider edges (wide 8px hit zone)
        if let InputEvent::MouseDown { button: input::MouseButton::Left, x, y, .. } = input_event {
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
        if *key == Key::S && modifiers.only_ctrl() {
            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                if !view_state.staging_well.has_text_focus() {
                    view_state.pending_messages.push(AppMessage::StashPush);
                    return true;
                }
            }
        }
        // Ctrl+Shift+S: stash pop
        if *key == Key::S && modifiers.ctrl_shift() {
            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                view_state.pending_messages.push(AppMessage::StashPop);
                return true;
            }
        }
        // Ctrl+Shift+A: toggle amend mode
        if *key == Key::A && modifiers.ctrl_shift() {
            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                if !view_state.staging_well.has_text_focus() {
                    view_state.pending_messages.push(AppMessage::ToggleAmend);
                    return true;
                }
            }
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
            if let Some(idx) = wt_index {
                if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                    if view_state.staging_well.has_worktree_selector()
                        && idx < view_state.staging_well.worktree_count()
                    {
                        view_state.switch_to_worktree(idx);
                        return true;
                    }
                }
            }
        }
        // Ctrl+Shift+F: Fetch
        if *key == Key::F && modifiers.ctrl_shift() {
            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                view_state.pending_messages.push(AppMessage::Fetch);
                return true;
            }
        }
        // Ctrl+Shift+L: Pull
        if *key == Key::L && modifiers.ctrl_shift() {
            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                view_state.pending_messages.push(AppMessage::Pull);
                return true;
            }
        }
        // Ctrl+Shift+P: Push
        if *key == Key::P && modifiers.ctrl_shift() {
            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                view_state.pending_messages.push(AppMessage::Push);
                return true;
            }
        }
        // Ctrl+Shift+R: Pull --rebase
        if *key == Key::R && modifiers.ctrl_shift() {
            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                view_state.pending_messages.push(AppMessage::PullRebase);
                return true;
            }
        }
        // Backtick (`): Open terminal at repo workdir
        if *key == Key::Grave && !modifiers.any() {
            if let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) {
                // Don't fire when a text input has focus
                if !view_state.staging_well.has_text_focus()
                    && !view_state.branch_sidebar.has_text_focus()
                {
                    if let Some(ref repo) = repo_tab.repo {
                        let path = repo.git_command_dir();
                        open_terminal_at(&path.to_string_lossy(), "repo", &mut self.toast_manager);
                    }
                    return true;
                }
            }
        }

        false
    }

    /// Handle a sidebar action by dispatching to the appropriate pending message or dialog.
    fn handle_sidebar_action(&mut self, action: SidebarAction) {
        let Some((_repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else { return };

        match action {
            SidebarAction::Checkout(name) => {
                view_state.pending_messages.push(AppMessage::CheckoutBranch(name));
            }
            SidebarAction::CheckoutRemote(remote, branch) => {
                view_state.pending_messages.push(AppMessage::CheckoutRemoteBranch(remote, branch));
            }
            SidebarAction::Delete(name) => {
                self.confirm_dialog.show("Delete Branch", &format!("Delete local branch '{}'?", name));
                self.pending_confirm_action = Some(AppMessage::DeleteBranch(name));
            }
            SidebarAction::ApplyStash(index) => {
                view_state.pending_messages.push(AppMessage::StashApply(index));
            }
            SidebarAction::DropStash(index) => {
                self.confirm_dialog.show("Drop Stash", &format!("Drop stash@{{{}}}? This cannot be undone.", index));
                self.pending_confirm_action = Some(AppMessage::StashDrop(index));
            }
            SidebarAction::DeleteTag(name) => {
                self.confirm_dialog.show("Delete Tag", &format!("Delete tag '{}'?", name));
                self.pending_confirm_action = Some(AppMessage::DeleteTag(name));
            }
        }
    }
}

/// Refresh commits, branch tips, tags, and header info from the repo.
/// Call this after any operation that changes branches, commits, or remote state.
/// Refreshes commits, branches, tags, header, etc. Returns an optional receiver for
/// async diff stats computation that should be stored in `App::diff_stats_receiver`.
fn refresh_repo_state(repo_tab: &mut RepoTab, view_state: &mut TabViewState, toast_manager: &mut ToastManager) -> Option<Receiver<Vec<(Oid, usize, usize)>>> {
    let Some(ref repo) = repo_tab.repo else { return None };

    // Preserve existing diff stats so they don't flicker away during refresh
    let prev_stats: HashMap<Oid, (usize, usize)> = repo_tab.commits.iter()
        .filter(|c| c.insertions > 0 || c.deletions > 0)
        .map(|c| (c.id, (c.insertions, c.deletions)))
        .collect();

    match repo.commit_graph(MAX_COMMITS) {
        Ok(commits) => {
            repo_tab.commits = commits;
        }
        Err(e) => {
            toast_manager.push(
                format!("Failed to load commits: {}", e),
                ToastSeverity::Error,
            );
            repo_tab.commits = Vec::new();
        }
    }

    // Restore cached diff stats until async task provides fresh values
    for commit in &mut repo_tab.commits {
        if let Some(&(ins, del)) = prev_stats.get(&commit.id) {
            commit.insertions = ins;
            commit.deletions = del;
        }
    }
    let head_oid = repo.head_oid().ok();
    view_state.commit_graph_view.head_oid = head_oid;

    let branch_tips = repo.branch_tips().unwrap_or_else(|e| {
        toast_manager.push(format!("Failed to load branches: {}", e), ToastSeverity::Error);
        Vec::new()
    });
    let tags = repo.tags().unwrap_or_else(|e| {
        toast_manager.push(format!("Failed to load tags: {}", e), ToastSeverity::Error);
        Vec::new()
    });
    let current = repo.current_branch().unwrap_or_else(|e| {
        toast_manager.push(format!("Failed to get current branch: {}", e), ToastSeverity::Error);
        String::new()
    });

    let worktrees = repo.worktrees().unwrap_or_else(|e| {
        toast_manager.push(format!("Failed to load worktrees: {}", e), ToastSeverity::Error);
        Vec::new()
    });

    // Insert synthetic "uncommitted changes" entries sorted by time
    {
        let synthetics = git::create_synthetic_entries(repo, &worktrees, &repo_tab.commits);
        if !synthetics.is_empty() {
            git::insert_synthetics_sorted(&mut repo_tab.commits, synthetics);
        }
    }

    view_state.commit_graph_view.update_layout(&repo_tab.commits);
    view_state.commit_graph_view.branch_tips = branch_tips.clone();
    view_state.commit_graph_view.tags = tags.clone();
    view_state.commit_graph_view.worktrees = worktrees.clone();
    view_state.branch_sidebar.set_branch_data(&branch_tips, &tags, current.clone());
    view_state.staging_well.set_worktrees(&worktrees);
    view_state.staging_well.current_branch = current.clone();
    // Prune cached worktree repos for paths that no longer exist
    let valid_paths: std::collections::HashSet<PathBuf> = worktrees.iter()
        .map(|wt| PathBuf::from(&wt.path))
        .collect();
    view_state.worktree_repo_cache.retain(|path, _| valid_paths.contains(path));
    view_state.worktrees = worktrees;

    let submodules = repo.submodules().unwrap_or_else(|e| {
        toast_manager.push(format!("Failed to load submodules: {}", e), ToastSeverity::Error);
        Vec::new()
    });
    view_state.staging_well.set_submodules(submodules);

    view_state.branch_sidebar.stashes = repo.stash_list();

    // When inside a submodule, override staging well with parent's siblings
    // so users can navigate between sibling submodules
    if let Some(ref focus) = view_state.submodule_focus {
        if let Some(parent) = focus.parent_stack.last() {
            view_state.staging_well.set_submodules(parent.parent_submodules.clone());
        }
    }

    let (ahead, behind) = repo.ahead_behind().unwrap_or_else(|e| {
        toast_manager.push(format!("Failed to compute ahead/behind: {}", e), ToastSeverity::Error);
        (0, 0)
    });
    view_state.header_bar.set_repo_info(
        view_state.header_bar.repo_name.clone(),
        current,
        ahead,
        behind,
    );

    // Update operation state (merge/rebase/cherry-pick in progress)
    view_state.header_bar.operation_state_label = git::repo_state_label(repo.repo_state());

    // Spawn async diff stats computation (skip synthetic entries — no real git object)
    let real_oids: Vec<Oid> = repo_tab.commits.iter()
        .filter(|c| !c.is_synthetic)
        .map(|c| c.id)
        .collect();
    if !real_oids.is_empty() {
        Some(repo.compute_diff_stats_async(real_oids))
    } else {
        None
    }
}

/// Initialize a tab's view state from its repo data
fn init_tab_view(repo_tab: &mut RepoTab, view_state: &mut TabViewState, text_renderer: &TextRenderer, scale: f32, toast_manager: &mut ToastManager) -> Option<Receiver<Vec<(Oid, usize, usize)>>> {
    // Sync view metrics to the current text renderer scale
    view_state.commit_graph_view.sync_metrics(text_renderer);
    view_state.branch_sidebar.sync_metrics(text_renderer);
    view_state.staging_well.set_scale(scale);

    if let Some(ref repo) = repo_tab.repo {
        // Set initial repo name in header (refresh_repo_state preserves the existing name)
        let repo_name = repo.repo_name();
        view_state.header_bar.set_repo_info(
            repo_name,
            repo.current_branch().unwrap_or_else(|_| "unknown".to_string()),
            0, 0,
        );
    }

    // Refresh commits, branches, tags, head, status, submodules, worktrees, stashes
    // (refresh_repo_state handles all of these — no need to call them separately)
    let rx = refresh_repo_state(repo_tab, view_state, toast_manager);

    // Start filesystem watcher for auto-refresh
    start_watcher(repo_tab, view_state, toast_manager);

    rx
}

/// Start (or restart) a filesystem watcher for the given tab's repo.
fn start_watcher(repo_tab: &RepoTab, view_state: &mut TabViewState, toast_manager: &mut ToastManager) {
    // Drop any existing watcher first
    view_state.watcher = None;
    view_state.watcher_rx = None;

    let Some(ref repo) = repo_tab.repo else { return };
    let Some(workdir) = repo.workdir() else { return };
    let git_dir = repo.git_dir();

    match RepoWatcher::new(workdir, git_dir, &view_state.worktrees) {
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
fn enter_submodule(
    name: &str,
    repo_tab: &mut RepoTab,
    view_state: &mut TabViewState,
    text_renderer: &TextRenderer,
    scale: f32,
    toast_manager: &mut ToastManager,
) -> bool {
    if repo_tab.repo.is_none() { return false; }

    // Find the submodule info by name
    let sm = view_state.staging_well.submodules.iter()
        .find(|s| s.name == name)
        .cloned();
    let Some(sm) = sm else {
        toast_manager.push(format!("Submodule '{}' not found", name), ToastSeverity::Error);
        return false;
    };

    // Resolve submodule path relative to the active worktree's workdir
    let parent_workdir = match view_state.staging_well.active_worktree_context() {
        Some(ctx) => ctx.path.clone(),
        None => {
            toast_manager.push("No active worktree".to_string(), ToastSeverity::Error);
            return false;
        }
    };
    let sub_path = parent_workdir.join(&sm.path);

    // Open the submodule as a repo
    let sub_repo = match GitRepo::open(&sub_path) {
        Ok(r) => r,
        Err(e) => {
            toast_manager.push(
                format!("Cannot open submodule '{}': {}", name, e),
                ToastSeverity::Error,
            );
            return false;
        }
    };

    // Save parent state
    let parent_repo = repo_tab.repo.take().unwrap();
    let parent_commits = std::mem::take(&mut repo_tab.commits);
    let parent_name = repo_tab.name.clone();
    let parent_submodules = view_state.staging_well.submodules.clone();

    let saved = SavedParentState {
        repo: parent_repo,
        commits: parent_commits,
        repo_name: parent_name,
        graph_scroll_offset: view_state.commit_graph_view.scroll_offset,
        selected_commit: view_state.commit_graph_view.selected_commit,
        sidebar_scroll_offset: view_state.branch_sidebar.scroll_offset,
        submodule_name: name.to_string(),
        parent_submodules,
    };

    // Clear diff/detail views
    view_state.diff_view.clear();
    view_state.commit_detail_view.clear();
    view_state.last_diff_commit = None;
    view_state.worktree_repo_cache.clear();
    view_state.active_worktree_path = None;

    // Swap in submodule data
    let sub_commits = sub_repo.commit_graph(MAX_COMMITS).unwrap_or_default();
    repo_tab.name = name.to_string();
    repo_tab.commits = sub_commits;
    repo_tab.repo = Some(sub_repo);

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
    let _ = init_tab_view(repo_tab, view_state, text_renderer, scale, toast_manager);

    true
}

/// Pop one level from the submodule focus stack, restoring parent state.
/// Returns true if we popped successfully.
fn exit_submodule(
    repo_tab: &mut RepoTab,
    view_state: &mut TabViewState,
    text_renderer: &TextRenderer,
    scale: f32,
    toast_manager: &mut ToastManager,
) -> bool {
    // Pop saved state from the focus stack (release borrow before init_tab_view)
    let saved = {
        let focus = match &mut view_state.submodule_focus {
            Some(f) => f,
            None => return false,
        };
        match focus.parent_stack.pop() {
            Some(s) => s,
            None => return false,
        }
    };

    // Clear diff/detail
    view_state.diff_view.clear();
    view_state.commit_detail_view.clear();
    view_state.last_diff_commit = None;
    view_state.worktree_repo_cache.clear();
    view_state.active_worktree_path = None;

    // Restore parent data
    let scroll_offset = saved.graph_scroll_offset;
    let selected = saved.selected_commit;
    let sidebar_scroll = saved.sidebar_scroll_offset;
    let parent_submodules = saved.parent_submodules;

    repo_tab.repo = Some(saved.repo);
    repo_tab.commits = saved.commits;
    repo_tab.name = saved.repo_name;

    // Re-init views with parent data
    let _ = init_tab_view(repo_tab, view_state, text_renderer, scale, toast_manager);

    // Restore scroll/selection
    view_state.commit_graph_view.scroll_offset = scroll_offset;
    view_state.commit_graph_view.selected_commit = selected;
    view_state.branch_sidebar.scroll_offset = sidebar_scroll;

    // Restore submodule siblings in staging well
    view_state.staging_well.set_submodules(parent_submodules);

    // If stack is now empty, clear focus entirely
    let stack_empty = view_state.submodule_focus.as_ref()
        .map(|f| f.parent_stack.is_empty())
        .unwrap_or(true);
    if stack_empty {
        view_state.submodule_focus = None;
    } else if let Some(ref mut focus) = view_state.submodule_focus {
        // Update current_name to the parent that's now active
        focus.current_name = focus.parent_stack.last()
            .map(|s| s.submodule_name.clone())
            .unwrap_or_default();
    }

    true
}

/// Pop multiple levels to reach the given depth (0 = root).
fn exit_to_depth(
    depth: usize,
    repo_tab: &mut RepoTab,
    view_state: &mut TabViewState,
    text_renderer: &TextRenderer,
    scale: f32,
    toast_manager: &mut ToastManager,
) {
    let current_depth = view_state.submodule_focus.as_ref()
        .map(|f| f.parent_stack.len())
        .unwrap_or(0);
    if depth >= current_depth {
        return;
    }
    let pops = current_depth - depth;
    for _ in 0..pops {
        if !exit_submodule(repo_tab, view_state, text_renderer, scale, toast_manager) {
            break;
        }
    }
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
            .chain(["kitty", "alacritty", "wezterm", "foot", "xterm", "gnome-terminal", "konsole"]
                .iter().map(|s| s.to_string()))
            .collect()
    } else {
        ["kitty", "alacritty", "wezterm", "foot", "xterm", "gnome-terminal", "konsole"]
            .iter().map(|s| s.to_string()).collect()
    };

    for terminal in &candidates {
        let result = if terminal == "gnome-terminal" {
            Command::new(terminal)
                .arg("--working-directory")
                .arg(dir)
                .spawn()
        } else {
            // Most terminals accept --working-directory or use the cwd
            Command::new(terminal)
                .current_dir(dir)
                .spawn()
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
            && let Err(e) = self.init_state(event_loop) {
                eprintln!("Failed to initialize: {e:?}");
                event_loop.exit();
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
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(_) => {
                state.surface.needs_recreate = true;
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                state.scale_factor = scale_factor;
                state.text_renderer.set_render_scale(scale_factor);
                state.bold_text_renderer.set_render_scale(scale_factor);
                for (repo_tab, view_state) in &mut self.tabs {
                    view_state.commit_graph_view.sync_metrics(&state.text_renderer);
                    view_state.commit_graph_view.update_layout(&repo_tab.commits);
                    view_state.branch_sidebar.sync_metrics(&state.text_renderer);
                    view_state.staging_well.set_scale(scale_factor as f32);
                }
                state.surface.needs_recreate = true;
            }

            WindowEvent::RedrawRequested => {
                // Poll async diff stats FIRST — apply completed results before
                // watcher or remote ops can orphan the receiver with a new one
                self.poll_diff_stats();
                // Re-launch diff stats if the previous receiver was orphaned
                self.ensure_diff_stats();
                // Poll filesystem watcher for external changes
                self.poll_watcher();
                // Poll background remote operations
                self.poll_remote_ops();
                // Process any pending messages
                self.process_messages();
                // Check if staging well requested an immediate status refresh (e.g., worktree switch)
                if let Some((_rt, vs)) = self.tabs.get_mut(self.active_tab) {
                    if vs.staging_well.status_refresh_needed {
                        self.status_dirty = true;
                        vs.staging_well.status_refresh_needed = false;
                    }
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

                // Poll native file picker for results
                self.repo_dialog.poll_picker();

                // Check for repo dialog actions
                if let Some(action) = self.repo_dialog.take_action() {
                    match action {
                        RepoDialogAction::Open(path) => {
                            let path_str = path.to_string_lossy().to_string();
                            self.config.add_recent_repo(&path_str);
                            self.open_repo_tab(path);
                        }
                        RepoDialogAction::Cancel => {}
                    }
                }

                if let Err(e) = draw_frame(self) {
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
                }

                let Some(state) = &self.state else { return };
                state.window.request_redraw();
            }

            // Handle input events
            ref win_event => {
                // Convert winit event to our InputEvent (brief mutable borrow)
                let input_event = state.input_state.handle_window_event(win_event);
                if let Some(input_event) = input_event {
                    self.handle_input_event(event_loop, &input_event);
                }
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(state) = &self.state {
            state.window.request_redraw();
        }
    }
}

impl App {
    /// Dispatch an input event to the appropriate handler.
    fn handle_input_event(&mut self, event_loop: &ActiveEventLoop, input_event: &InputEvent) {
        let Some(ref state) = self.state else { return };

        // Calculate layout
        let extent = state.surface.extent();
        let screen_bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
        let scale = state.scale_factor as f32;
        let tab_bar_height = if self.tabs.len() > 1 { TabBar::height(scale) } else { 0.0 };
        let (tab_bar_bounds, main_bounds) = screen_bounds.take_top(tab_bar_height);
        let layout = ScreenLayout::compute_with_ratios_and_shortcut(
            main_bounds, 4.0, scale,
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
            && self.tab_bar.handle_event(input_event, tab_bar_bounds).is_consumed() {
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
        let Some((_repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else { return };

        // Handle per-tab global keys (except Tab, which is handled after panel routing)
        if let InputEvent::KeyDown { key, .. } = input_event
            && key == &Key::Escape {
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
        if view_state.branch_sidebar.handle_event(input_event, layout.sidebar).is_consumed() {
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
        let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) else { return };
        if view_state.header_bar.handle_event(input_event, layout.header).is_consumed() {
            if let Some(action) = view_state.header_bar.take_action() {
                use crate::ui::widgets::HeaderAction;
                match action {
                    HeaderAction::Fetch => {
                        view_state.pending_messages.push(AppMessage::Fetch);
                    }
                    HeaderAction::Pull => {
                        view_state.pending_messages.push(AppMessage::Pull);
                    }
                    HeaderAction::PullRebase => {
                        view_state.pending_messages.push(AppMessage::PullRebase);
                    }
                    HeaderAction::Push => {
                        view_state.pending_messages.push(AppMessage::Push);
                    }
                    HeaderAction::Commit => {
                        view_state.focused_panel = FocusedPanel::RightPanel;
                        view_state.right_panel_mode = RightPanelMode::Staging;
                    }
                    HeaderAction::Help => {
                        self.shortcut_bar_visible = !self.shortcut_bar_visible;
                        self.config.shortcut_bar_visible = self.shortcut_bar_visible;
                        self.config.save();
                    }
                    HeaderAction::Settings => {
                        self.settings_dialog.show();
                    }
                    HeaderAction::BreadcrumbNav(depth) => {
                        view_state.pending_messages.push(AppMessage::ExitToDepth(depth));
                    }
                    HeaderAction::BreadcrumbClose => {
                        view_state.pending_messages.push(AppMessage::ExitToDepth(0));
                    }
                    HeaderAction::AbortOperation => {
                        view_state.pending_messages.push(AppMessage::AbortOperation);
                    }
                }
            }
            return;
        }

        // Route events to right panel content (commit detail + diff view)
        {
            let pill_bar_h = view_state.staging_well.pill_bar_height();
            let (_pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);

            // Route to commit detail view when in browse mode
            if view_state.right_panel_mode == RightPanelMode::Browse
                && view_state.commit_detail_view.has_content()
            {
                let (detail_rect, _diff_rect) = content_rect.split_vertical(0.40);
                if view_state.commit_detail_view.handle_event(input_event, detail_rect).is_consumed() {
                    if let Some(action) = view_state.commit_detail_view.take_action() {
                        match action {
                            CommitDetailAction::ViewFileDiff(oid, path) => {
                                view_state.pending_messages.push(AppMessage::ViewCommitFileDiff(oid, path));
                            }
                            CommitDetailAction::OpenSubmodule(name) => {
                                view_state.pending_messages.push(AppMessage::EnterSubmodule(name));
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
                        let (_staging_rect, diff_rect) = content_rect.split_vertical(0.45);
                        let (_hdr, body) = diff_rect.take_top(header_h);
                        body
                    }
                    _ => {
                        let (_hdr, body) = content_rect.take_top(header_h);
                        body
                    }
                };
                if view_state.diff_view.handle_event(input_event, diff_bounds).is_consumed() {
                    if let Some(action) = view_state.diff_view.take_action() {
                        match action {
                            DiffAction::StageHunk(path, hunk_idx) => {
                                view_state.pending_messages.push(AppMessage::StageHunk(path, hunk_idx));
                            }
                            DiffAction::UnstageHunk(path, hunk_idx) => {
                                view_state.pending_messages.push(AppMessage::UnstageHunk(path, hunk_idx));
                            }
                        }
                    }
                    return;
                }
            }
        }

        // Right-click context menus
        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else { return };
        if let InputEvent::MouseDown { button: input::MouseButton::Right, x, y, .. } = input_event {
            // Check which panel was right-clicked and show context menu
            if layout.graph.contains(*x, *y) {
                if let Some((items, oid)) = view_state.commit_graph_view.context_menu_items_at(
                    *x, *y, &repo_tab.commits, layout.graph,
                ) {
                    view_state.context_menu_commit = Some(oid);
                    view_state.context_menu.show(items, *x, *y);
                    return;
                }
            } else if layout.sidebar.contains(*x, *y) {
                if let Some(items) = view_state.branch_sidebar.context_menu_items_at(*x, *y, layout.sidebar) {
                    view_state.context_menu.show(items, *x, *y);
                    return;
                }
            } else if layout.right_panel.contains(*x, *y) {
                // Check pill bar first
                let pill_bar_h = view_state.staging_well.pill_bar_height();
                let (pill_rect, _) = layout.right_panel.take_top(pill_bar_h);
                if pill_rect.contains(*x, *y) {
                    if let Some(items) = view_state.staging_well.pill_context_menu_at(*x, *y) {
                        view_state.context_menu.show(items, *x, *y);
                        return;
                    }
                }
                // Then check staging file lists
                if view_state.right_panel_mode == RightPanelMode::Staging {
                    if let Some(items) = view_state.staging_well.context_menu_items_at(*x, *y, layout.right_panel) {
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
                || view_state.right_panel_mode != RightPanelMode::Staging {
                view_state.staging_well.unfocus_all();
            }
        }

        // Handle worktree pill bar clicks (before content routing)
        if let InputEvent::MouseDown { .. } = input_event {
            let pill_bar_h = view_state.staging_well.pill_bar_height();
            let (pill_rect, _content_rect) = layout.right_panel.take_top(pill_bar_h);
            if view_state.staging_well.handle_pill_event(input_event, pill_rect).is_consumed() {
                if let Some(action) = view_state.staging_well.take_action() {
                    if let StagingAction::SwitchWorktree(index) = action {
                        view_state.switch_to_worktree(index);
                    }
                }
                return;
            }
        }

        // Route scroll events to the panel under the mouse cursor (hover-based, not focus-based)
        if let InputEvent::Scroll { x, y, .. } = input_event {
            if layout.graph.contains(*x, *y) {
                let prev_selected = view_state.commit_graph_view.selected_commit;
                let response = view_state.commit_graph_view.handle_event(input_event, &repo_tab.commits, layout.graph);
                if view_state.commit_graph_view.selected_commit != prev_selected
                    && let Some(oid) = view_state.commit_graph_view.selected_commit
                        && view_state.last_diff_commit != Some(oid) {
                            if let Some(synthetic) = repo_tab.commits.iter().find(|c| c.id == oid && c.is_synthetic) {
                                // Synthetic row: switch to that worktree if named
                                if let Some(wt_name) = synthetic.synthetic_wt_name.clone() {
                                    view_state.switch_to_worktree_by_name(&wt_name);
                                } else {
                                    // Single-worktree: enter staging mode directly
                                    view_state.right_panel_mode = RightPanelMode::Staging;
                                    view_state.last_diff_commit = None;
                                    view_state.commit_detail_view.clear();
                                    view_state.diff_view.clear();
                                }
                            } else {
                                view_state.pending_messages.push(AppMessage::SelectedCommit(oid));
                            }
                        }
                if let Some(action) = view_state.commit_graph_view.take_action() {
                    view_state.handle_graph_action(action);
                }
                if response.is_consumed() {
                    return;
                }
            } else if layout.right_panel.contains(*x, *y)
                && view_state.right_panel_mode == RightPanelMode::Staging {
                let pill_bar_h = view_state.staging_well.pill_bar_height();
                let (_pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);
                let (staging_rect, _diff_rect) = content_rect.split_vertical(0.45);
                let response = view_state.staging_well.handle_event(input_event, staging_rect);
                if let Some(action) = view_state.staging_well.take_action() {
                    view_state.handle_staging_action(action);
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
                let response = view_state.commit_graph_view.handle_event(input_event, &repo_tab.commits, layout.graph);
                if view_state.commit_graph_view.selected_commit != prev_selected
                    && let Some(oid) = view_state.commit_graph_view.selected_commit
                        && view_state.last_diff_commit != Some(oid) {
                            if let Some(synthetic) = repo_tab.commits.iter().find(|c| c.id == oid && c.is_synthetic) {
                                if let Some(wt_name) = synthetic.synthetic_wt_name.clone() {
                                    view_state.switch_to_worktree_by_name(&wt_name);
                                } else {
                                    // Single-worktree: enter staging mode directly
                                    view_state.right_panel_mode = RightPanelMode::Staging;
                                    view_state.last_diff_commit = None;
                                    view_state.commit_detail_view.clear();
                                    view_state.diff_view.clear();
                                }
                            } else {
                                view_state.pending_messages.push(AppMessage::SelectedCommit(oid));
                            }
                        }
                if let Some(action) = view_state.commit_graph_view.take_action() {
                    view_state.handle_graph_action(action);
                }
                if response.is_consumed() {
                    return;
                }
            }
            FocusedPanel::RightPanel => {
                if view_state.right_panel_mode == RightPanelMode::Staging {
                    let pill_bar_h = view_state.staging_well.pill_bar_height();
                    let (_pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);
                    let (staging_rect, _diff_rect) = content_rect.split_vertical(0.45);
                    let response = view_state.staging_well.handle_event(input_event, staging_rect);

                    if let Some(action) = view_state.staging_well.take_action() {
                        view_state.handle_staging_action(action);
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
            view_state.branch_sidebar.set_focused(view_state.focused_panel == FocusedPanel::Sidebar);
            if view_state.focused_panel != FocusedPanel::RightPanel
                || view_state.right_panel_mode != RightPanelMode::Staging {
                view_state.staging_well.unfocus_all();
            }
            return;
        }

        // Update hover states
        if let InputEvent::MouseMove { x, y, .. } = input_event {
            view_state.header_bar.update_hover(*x, *y, layout.header);
            view_state.branch_sidebar.update_hover(*x, *y, layout.sidebar);
            {
                let pill_bar_h = view_state.staging_well.pill_bar_height();
                let (_pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);
                let (staging_rect, _diff_rect) = content_rect.split_vertical(0.45);
                view_state.staging_well.update_hover(*x, *y, staging_rect);
            }

            if let Some(ref render_state) = self.state {
                if tab_count > 1 {
                    self.tab_bar.update_hover_with_renderer(*x, *y, tab_bar_bounds, &render_state.text_renderer);
                }

                let cursor = determine_cursor(*x, *y, &layout, view_state, &self.tab_bar, tab_count);
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
    }

    // -- Text cursor: text input fields --

    // Staging area text inputs (subject line, body area) - only in staging mode
    if layout.right_panel.contains(x, y) && view_state.right_panel_mode == RightPanelMode::Staging {
        let pill_bar_h = view_state.staging_well.pill_bar_height(); // scale already in pill_bar_height
        let (_pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);
        let (staging_rect, _diff_rect) = content_rect.split_vertical(0.45);
        let (_, _, subject_bounds, body_bounds, _) = view_state.staging_well.compute_regions(staging_rect);
        if subject_bounds.contains(x, y) || body_bounds.contains(x, y) {
            return CursorIcon::Text;
        }
    }

    // Search bar when active (overlays the graph area)
    if view_state.commit_graph_view.search_bar.is_active() && layout.graph.contains(x, y) {
        let scrollbar_width = 8.0;
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
    if layout.sidebar.contains(x, y) && view_state.branch_sidebar.is_over_filter_bar(x, y, layout.sidebar) {
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

/// Classify a git error message and return a more helpful description.
/// Returns `(friendly_message, is_rejected_push)`.
fn classify_git_error(op: &str, stderr: &str) -> (String, bool) {
    let lower = stderr.to_lowercase();
    let is_rejected = lower.contains("rejected") || lower.contains("non-fast-forward");

    let friendly = if lower.contains("terminal prompts disabled") || lower.contains("could not read username") {
        format!("{} failed: Authentication required. Configure SSH keys or a credential helper.", op)
    } else if lower.contains("permission denied") {
        format!("{} failed: Permission denied. Check your SSH key or access token.", op)
    } else if lower.contains("could not read password") {
        format!("{} failed: Password required. Set up a credential helper (git config credential.helper cache).", op)
    } else if lower.contains("host key verification failed") {
        format!("{} failed: SSH host key not trusted. Run ssh-keyscan to add the host.", op)
    } else if lower.contains("repository not found") || lower.contains("404") {
        format!("{} failed: Repository not found. Check the remote URL.", op)
    } else if lower.contains("connection refused") || lower.contains("could not resolve") {
        format!("{} failed: Cannot connect to remote. Check your network and remote URL.", op)
    } else if is_rejected {
        format!("{} rejected: Remote has new commits. Pull first, or use Force Push.", op)
    } else {
        // Show up to 3 lines of the error for context
        let error_summary: String = stderr.lines().take(3).collect::<Vec<_>>().join("\n");
        if error_summary.is_empty() {
            format!("{} failed: unknown error", op)
        } else {
            format!("{} failed: {}", op, error_summary)
        }
    };

    (friendly, is_rejected)
}

/// Handle a context menu action by dispatching to the appropriate AppMessage
fn handle_context_menu_action(
    action_id: &str,
    view_state: &mut TabViewState,
    toast_manager: &mut ToastManager,
    confirm_dialog: &mut ConfirmDialog,
    branch_name_dialog: &mut BranchNameDialog,
    remote_dialog: &mut RemoteDialog,
    repo: Option<&crate::git::GitRepo>,
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
                        toast_manager.push(
                            format!("Copied: {}", &sha[..7]),
                            ToastSeverity::Success,
                        );
                    }
                    Err(e) => {
                        toast_manager.push(
                            format!("Clipboard error: {e}"),
                            ToastSeverity::Error,
                        );
                    }
                }
            }
        }
        "view_details" => {
            if let Some(oid) = view_state.context_menu_commit {
                view_state.pending_messages.push(AppMessage::SelectedCommit(oid));
            }
        }
        "checkout" => {
            if param.is_empty() {
                // Commit graph checkout: find the branch at the selected commit
                if let Some(oid) = view_state.context_menu_commit
                    && let Some(tip) = view_state.commit_graph_view.branch_tips.iter()
                        .find(|t| t.oid == oid && !t.is_remote)
                    {
                        view_state.pending_messages.push(AppMessage::CheckoutBranch(tip.name.clone()));
                    }
            } else {
                // Branch sidebar checkout
                view_state.pending_messages.push(AppMessage::CheckoutBranch(param.to_string()));
            }
        }
        "checkout_remote" => {
            if let Some((remote, branch)) = param.split_once('/') {
                view_state.pending_messages.push(AppMessage::CheckoutRemoteBranch(
                    remote.to_string(),
                    branch.to_string(),
                ));
            }
        }
        "delete" => {
            if !param.is_empty() {
                confirm_dialog.show("Delete Branch", &format!("Delete local branch '{}'?", param));
                *pending_confirm_action = Some(AppMessage::DeleteBranch(param.to_string()));
            }
        }
        "push" => {
            view_state.pending_messages.push(AppMessage::Push);
        }
        "pull" => {
            view_state.pending_messages.push(AppMessage::Pull);
        }
        "pull_rebase" => {
            view_state.pending_messages.push(AppMessage::PullRebase);
        }
        "force_push" => {
            confirm_dialog.show("Force Push", "Force push with --force-with-lease? This may overwrite remote commits.");
            *pending_confirm_action = Some(AppMessage::PushForce);
        }
        // Staging actions
        "stage" => {
            if !param.is_empty() {
                view_state.pending_messages.push(AppMessage::StageFile(param.to_string()));
            }
        }
        "unstage" => {
            if !param.is_empty() {
                view_state.pending_messages.push(AppMessage::UnstageFile(param.to_string()));
            }
        }
        "view_diff" => {
            if !param.is_empty() {
                let staged = view_state.staging_well.staged_list.files
                    .iter().any(|f| f.path == param);
                view_state.pending_messages.push(AppMessage::ViewDiff(param.to_string(), staged));
            }
        }
        "discard" => {
            if !param.is_empty() {
                confirm_dialog.show("Discard Changes", &format!("Discard changes to '{}'? This cannot be undone.", param));
                *pending_confirm_action = Some(AppMessage::DiscardFile(param.to_string()));
            }
        }
        "delete_submodule" => {
            if !param.is_empty() {
                confirm_dialog.show("Delete Submodule", &format!("Remove submodule '{}'? This will deinit and remove it.", param));
                *pending_confirm_action = Some(AppMessage::DeleteSubmodule(param.to_string()));
            }
        }
        "update_submodule" => {
            if !param.is_empty() {
                view_state.pending_messages.push(AppMessage::UpdateSubmodule(param.to_string()));
            }
        }
        "enter_submodule" => {
            if !param.is_empty() {
                view_state.pending_messages.push(AppMessage::EnterSubmodule(param.to_string()));
            }
        }
        "open_submodule" => {
            if !param.is_empty() {
                let path = view_state.staging_well.submodules.iter()
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
                let path = view_state.worktrees.iter()
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
                view_state.switch_to_worktree_by_name(param);
            }
        }
        "jump_to_worktree" => {
            if !param.is_empty() {
                view_state.pending_messages.push(AppMessage::JumpToWorktreeBranch(param.to_string()));
            }
        }
        "remove_worktree" => {
            if !param.is_empty() {
                confirm_dialog.show("Remove Worktree", &format!("Remove worktree '{}'?", param));
                *pending_confirm_action = Some(AppMessage::RemoveWorktree(param.to_string()));
            }
        }
        "merge" => {
            if !param.is_empty() {
                confirm_dialog.show("Merge Branch", &format!("Merge '{}' into current branch?", param));
                *pending_confirm_action = Some(AppMessage::MergeBranch(param.to_string()));
            }
        }
        "rebase" => {
            if !param.is_empty() {
                confirm_dialog.show("Rebase Branch", &format!("Rebase current branch onto '{}'?", param));
                *pending_confirm_action = Some(AppMessage::RebaseBranch(param.to_string()));
            }
        }
        "cherry_pick" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                confirm_dialog.show("Cherry-pick", &format!("Cherry-pick commit {}?", short));
                *pending_confirm_action = Some(AppMessage::CherryPick(oid));
            }
        }
        "revert_commit" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                confirm_dialog.show("Revert Commit", &format!("Create a new commit that reverts {}?", short));
                *pending_confirm_action = Some(AppMessage::RevertCommit(oid));
            }
        }
        "reset_soft" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                confirm_dialog.show("Reset (Soft)", &format!("Reset to {}? Changes will be kept staged.", short));
                *pending_confirm_action = Some(AppMessage::ResetToCommit(oid, git2::ResetType::Soft));
            }
        }
        "reset_mixed" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                confirm_dialog.show("Reset (Mixed)", &format!("Reset to {}? Changes will be kept unstaged.", short));
                *pending_confirm_action = Some(AppMessage::ResetToCommit(oid, git2::ResetType::Mixed));
            }
        }
        "reset_hard" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                confirm_dialog.show("Reset (Hard)", &format!("Reset to {}?\n\nALL changes will be DISCARDED. This cannot be undone.", short));
                *pending_confirm_action = Some(AppMessage::ResetToCommit(oid, git2::ResetType::Hard));
            }
        }
        "create_branch" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                let default_name = format!("branch-{}", short);
                branch_name_dialog.show(&default_name, oid);
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
                view_state.pending_messages.push(AppMessage::StashApply(index));
            }
        }
        "pop_stash" => {
            if let Ok(index) = param.parse::<usize>() {
                view_state.pending_messages.push(AppMessage::StashPopIndex(index));
            }
        }
        "drop_stash" => {
            if let Ok(index) = param.parse::<usize>() {
                confirm_dialog.show("Drop Stash", &format!("Drop stash@{{{}}}? This cannot be undone.", index));
                *pending_confirm_action = Some(AppMessage::StashDrop(index));
            }
        }
        "add_remote" => {
            remote_dialog.show_add();
        }
        "edit_remote_url" => {
            if !param.is_empty() {
                let current_url = repo
                    .and_then(|r| r.remote_url(param))
                    .unwrap_or_default();
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
fn add_panel_chrome(output: &mut WidgetOutput, layout: &ScreenLayout, screen_bounds: &Rect, focused: FocusedPanel, mouse_pos: (f32, f32)) {
    // Panel backgrounds for depth separation
    output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
        &layout.graph,
        theme::PANEL_GRAPH.to_array(),
    ));
    output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
        &layout.right_panel,
        theme::PANEL_STAGING.to_array(),
    ));

    // Border below shortcut bar (full width of screen)
    output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
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
    // Subtle 1px line at rest, wider 2px highlighted line on hover
    if sidebar_graph_hover {
        output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
            &Rect::new(layout.sidebar.right(), layout.sidebar.y, 2.0, layout.sidebar.height),
            theme::BORDER_LIGHT.to_array(),
        ));
    } else {
        output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
            &Rect::new(layout.sidebar.right(), layout.sidebar.y, 1.0, layout.sidebar.height),
            theme::BORDER.with_alpha(0.35).to_array(),
        ));
    }

    // Vertical divider: graph | right panel
    if graph_right_hover {
        output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
            &Rect::new(layout.graph.right(), layout.graph.y, 2.0, layout.graph.height),
            theme::BORDER_LIGHT.to_array(),
        ));
    } else {
        output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
            &Rect::new(layout.graph.right(), layout.graph.y, 1.0, layout.graph.height),
            theme::BORDER.with_alpha(0.35).to_array(),
        ));
    }

    // Focused panel indicator: subtle accent-colored top border (2px at ~40% alpha)
    let focused_rect = match focused {
        FocusedPanel::Graph => &layout.graph,
        FocusedPanel::RightPanel => &layout.right_panel,
        FocusedPanel::Sidebar => &layout.sidebar,
    };
    output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
        &Rect::new(focused_rect.x, focused_rect.y, focused_rect.width, 2.0),
        theme::ACCENT.with_alpha(0.4).to_array(),
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
    repo_dialog: &RepoDialog,
    settings_dialog: &SettingsDialog,
    confirm_dialog: &ConfirmDialog,
    branch_name_dialog: &BranchNameDialog,
    remote_dialog: &RemoteDialog,
    text_renderer: &TextRenderer,
    bold_text_renderer: &TextRenderer,
    scale_factor: f64,
    extent: [u32; 2],
    avatar_cache: &mut AvatarCache,
    avatar_renderer: &AvatarRenderer,
    sidebar_ratio: f32,
    graph_ratio: f32,
    shortcut_bar_visible: bool,
    mouse_pos: (f32, f32),
    elapsed: f32,
) -> (WidgetOutput, WidgetOutput, WidgetOutput) {
    let screen_bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
    let scale = scale_factor as f32;

    // Tab bar takes space at top when multiple tabs
    let tab_bar_height = if tabs.len() > 1 { TabBar::height(scale) } else { 0.0 };
    let (tab_bar_bounds, main_bounds) = screen_bounds.take_top(tab_bar_height);
    let layout = ScreenLayout::compute_with_ratios_and_shortcut(
        main_bounds, 4.0, scale,
        Some(sidebar_ratio),
        Some(graph_ratio),
        shortcut_bar_visible,
    );

    // Three layers: graph content renders first, chrome on top, overlay on top of everything
    let mut graph_output = WidgetOutput::new();
    let mut chrome_output = WidgetOutput::new();
    let mut overlay_output = WidgetOutput::new();

    // Panel backgrounds and borders go in graph layer (base - renders first, behind everything)
    let focused = tabs.get(active_tab).map(|(_, vs)| vs.focused_panel).unwrap_or_default();
    add_panel_chrome(&mut graph_output, &layout, &main_bounds, focused, mouse_pos);

    // Active tab views
    if let Some((repo_tab, view_state)) = tabs.get_mut(active_tab) {
        // Commit graph (graph layer - renders first)
        let spline_vertices = view_state.commit_graph_view.layout_splines(text_renderer, &repo_tab.commits, layout.graph);
        let (text_vertices, pill_vertices, av_vertices) = view_state.commit_graph_view.layout_text(
            text_renderer, &repo_tab.commits, layout.graph,
            avatar_cache, avatar_renderer,
        );
        graph_output.spline_vertices.extend(spline_vertices);
        graph_output.spline_vertices.extend(pill_vertices);
        graph_output.text_vertices.extend(text_vertices);
        graph_output.avatar_vertices.extend(av_vertices);

        // Header bar (chrome layer - on top of graph)
        chrome_output.extend(view_state.header_bar.layout_with_bold(text_renderer, bold_text_renderer, layout.header, elapsed));

        // Shortcut bar (chrome layer - on top of graph) - only when visible
        if shortcut_bar_visible {
            chrome_output.extend(view_state.shortcut_bar.layout(text_renderer, layout.shortcut_bar));
        }

        // Branch sidebar (chrome layer)
        chrome_output.extend(view_state.branch_sidebar.layout(text_renderer, bold_text_renderer, layout.sidebar));

        // Right panel (chrome layer) - worktree pills + mode-dependent content
        {
            let pill_bar_h = view_state.staging_well.pill_bar_height();
            let (pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);

            // Worktree pill bar (visible when there are worktree contexts)
            if pill_bar_h > 0.0 {
                chrome_output.extend(view_state.staging_well.layout_worktree_pills(text_renderer, pill_rect));
            }

            match view_state.right_panel_mode {
                RightPanelMode::Staging => {
                    // Upper: staging well, Lower: diff view with header
                    let (staging_rect, diff_rect) = content_rect.split_vertical(0.45);
                    chrome_output.extend(view_state.staging_well.layout(text_renderer, staging_rect));

                    // Preview header bar
                    let header_h = 28.0 * scale;
                    let (header_rect, diff_body_rect) = diff_rect.take_top(header_h);
                    chrome_output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
                        &header_rect,
                        theme::SURFACE_RAISED.to_array(),
                    ));
                    let header_text_y = header_rect.y + (header_h - bold_text_renderer.line_height()) / 2.0;
                    let header_text_x = header_rect.x + 12.0 * scale;
                    if view_state.diff_view.has_content() {
                        let title = view_state.diff_view.title();
                        chrome_output.bold_text_vertices.extend(bold_text_renderer.layout_text(
                            title, header_text_x, header_text_y,
                            theme::TEXT_BRIGHT.to_array(),
                        ));
                    } else {
                        chrome_output.bold_text_vertices.extend(bold_text_renderer.layout_text(
                            "Preview", header_text_x, header_text_y,
                            theme::TEXT_MUTED.to_array(),
                        ));
                    }

                    if view_state.diff_view.has_content() {
                        chrome_output.extend(view_state.diff_view.layout(text_renderer, diff_body_rect));
                    } else {
                        let msg = "Select a file to preview its diff";
                        let msg_w = text_renderer.measure_text(msg);
                        let line_h = text_renderer.line_height();
                        let cx = diff_body_rect.x + (diff_body_rect.width - msg_w) / 2.0;
                        let cy = diff_body_rect.y + (diff_body_rect.height - line_h) / 2.0;
                        chrome_output.text_vertices.extend(text_renderer.layout_text(
                            msg, cx, cy,
                            theme::TEXT_MUTED.to_array(),
                        ));
                    }
                }
                RightPanelMode::Browse => {
                    // Upper: commit detail, Lower: diff view with header
                    if view_state.commit_detail_view.has_content() {
                        let (detail_rect, diff_rect) = content_rect.split_vertical(0.40);
                        chrome_output.extend(view_state.commit_detail_view.layout(text_renderer, detail_rect));

                        // Preview header bar
                        let header_h = 28.0 * scale;
                        let (header_rect, diff_body_rect) = diff_rect.take_top(header_h);
                        chrome_output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
                            &header_rect,
                            theme::SURFACE_RAISED.to_array(),
                        ));
                        let header_text_y = header_rect.y + (header_h - bold_text_renderer.line_height()) / 2.0;
                        let header_text_x = header_rect.x + 12.0 * scale;
                        if view_state.diff_view.has_content() {
                            let title = view_state.diff_view.title();
                            chrome_output.bold_text_vertices.extend(bold_text_renderer.layout_text(
                                title, header_text_x, header_text_y,
                                theme::TEXT_BRIGHT.to_array(),
                            ));
                        } else {
                            chrome_output.bold_text_vertices.extend(bold_text_renderer.layout_text(
                                "Diff", header_text_x, header_text_y,
                                theme::TEXT_MUTED.to_array(),
                            ));
                        }

                        if view_state.diff_view.has_content() {
                            chrome_output.extend(view_state.diff_view.layout(text_renderer, diff_body_rect));
                        }
                    } else if view_state.diff_view.has_content() {
                        // Preview header bar (full area, no commit detail)
                        let header_h = 28.0 * scale;
                        let (header_rect, diff_body_rect) = content_rect.take_top(header_h);
                        chrome_output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
                            &header_rect,
                            theme::SURFACE_RAISED.to_array(),
                        ));
                        let header_text_y = header_rect.y + (header_h - bold_text_renderer.line_height()) / 2.0;
                        let header_text_x = header_rect.x + 12.0 * scale;
                        let title = view_state.diff_view.title();
                        chrome_output.bold_text_vertices.extend(bold_text_renderer.layout_text(
                            title, header_text_x, header_text_y,
                            theme::TEXT_BRIGHT.to_array(),
                        ));
                        chrome_output.extend(view_state.diff_view.layout(text_renderer, diff_body_rect));
                    } else {
                        // Preview header bar (empty state)
                        let header_h = 28.0 * scale;
                        let (header_rect, body_rect) = content_rect.take_top(header_h);
                        chrome_output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
                            &header_rect,
                            theme::SURFACE_RAISED.to_array(),
                        ));
                        let header_text_y = header_rect.y + (header_h - bold_text_renderer.line_height()) / 2.0;
                        let header_text_x = header_rect.x + 12.0 * scale;
                        chrome_output.bold_text_vertices.extend(bold_text_renderer.layout_text(
                            "Preview", header_text_x, header_text_y,
                            theme::TEXT_MUTED.to_array(),
                        ));
                        let msg = "Select a commit to browse";
                        let msg_w = text_renderer.measure_text(msg);
                        let line_h = text_renderer.line_height();
                        let cx = body_rect.x + (body_rect.width - msg_w) / 2.0;
                        let cy = body_rect.y + (body_rect.height - line_h) / 2.0;
                        chrome_output.text_vertices.extend(text_renderer.layout_text(
                            msg, cx, cy,
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
        && view_state.context_menu.is_visible() {
            overlay_output.extend(view_state.context_menu.layout(text_renderer, screen_bounds));
        }

    // Toast notifications (overlay layer - on top of context menus)
    overlay_output.extend(toast_manager.layout(text_renderer, screen_bounds, scale));

    // Repo dialog (overlay layer - on top of everything including toasts)
    if repo_dialog.is_visible() {
        overlay_output.extend(repo_dialog.layout(text_renderer, screen_bounds));
    }

    // Settings dialog (overlay layer - on top of everything)
    if settings_dialog.is_visible() {
        overlay_output.extend(settings_dialog.layout(text_renderer, screen_bounds));
    }

    // Confirm dialog (overlay layer - on top of everything including settings)
    if confirm_dialog.is_visible() {
        overlay_output.extend(confirm_dialog.layout(text_renderer, screen_bounds));
    }

    // Branch name dialog (overlay layer - on top of everything)
    if branch_name_dialog.is_visible() {
        overlay_output.extend(branch_name_dialog.layout(text_renderer, screen_bounds));
    }

    // Remote dialog (overlay layer - on top of everything)
    if remote_dialog.is_visible() {
        overlay_output.extend(remote_dialog.layout(text_renderer, screen_bounds));
    }

    (graph_output, chrome_output, overlay_output)
}

fn draw_frame(app: &mut App) -> Result<()> {
    let state = app.state.as_mut().unwrap();
    state.previous_frame_end.as_mut().unwrap().cleanup_finished();

    // Recreate swapchain if needed
    if state.surface.needs_recreate {
        state.surface.recreate(&state.ctx, state.window.inner_size())?;
    }

    // Acquire next image
    let (image_index, suboptimal, acquire_future) =
        match acquire_next_image(state.surface.swapchain.clone(), None).map_err(Validated::unwrap) {
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
        view_state.header_bar.generic_op_label = view_state.generic_op_receiver
            .as_ref()
            .map(|(_, label, _)| {
                let dot_count = ((elapsed * 2.5) as usize % 3) + 1;
                let dots: String = ".".repeat(dot_count);
                format!("{}{}", label, dots)
            });
        view_state.header_bar.update_button_state(elapsed);
        view_state.staging_well.update_button_state();
        view_state.staging_well.update_cursors(now);
        view_state.commit_graph_view.search_bar.update_cursor(now);
        view_state.branch_sidebar.update_filter_cursor(now);
        view_state.shortcut_bar.set_context(match view_state.focused_panel {
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
            let mut segs: Vec<String> = focus.parent_stack.iter()
                .map(|s| s.repo_name.clone())
                .collect();
            segs.push(focus.current_name.clone());
            view_state.header_bar.breadcrumb_segments = segs;
        } else {
            view_state.header_bar.breadcrumb_segments.clear();
        }

        // Pre-compute breadcrumb segment bounds for hit testing
        // (needs approximate header bounds — compute from extent)
        let extent = state.surface.extent();
        let screen_bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
        let tab_bar_height = if single_tab { 0.0 } else { TabBar::height(state.scale_factor as f32) };
        let (_tb_bounds, main_bounds) = screen_bounds.take_top(tab_bar_height);
        let approx_layout = ScreenLayout::compute_with_ratios_and_shortcut(
            main_bounds, 4.0, state.scale_factor as f32,
            Some(app.sidebar_ratio),
            Some(app.graph_ratio),
            app.shortcut_bar_visible,
        );
        view_state.header_bar.update_breadcrumb_bounds(&state.text_renderer, approx_layout.header);
        view_state.header_bar.update_abort_bounds(&state.bold_text_renderer, approx_layout.header);
    }

    // Update toast manager
    app.toast_manager.update(Instant::now());

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
        &mut app.tabs, app.active_tab, &app.tab_bar,
        &mut app.toast_manager, &app.repo_dialog, &app.settings_dialog, &app.confirm_dialog, &app.branch_name_dialog, &app.remote_dialog,
        &state.text_renderer, &state.bold_text_renderer, scale_factor, extent,
        &mut state.avatar_cache, &state.avatar_renderer,
        sidebar_ratio, graph_ratio,
        app.shortcut_bar_visible,
        mouse_pos,
        elapsed,
    );

    let viewport = Viewport {
        offset: [0.0, 0.0],
        extent: [extent[0] as f32, extent[1] as f32],
        depth_range: 0.0..=1.0,
    };

    // Upload avatar atlas in a separate command buffer if dirty
    if state.avatar_renderer.needs_upload() {
        let mut upload_builder = AutoCommandBufferBuilder::primary(
            state.ctx.command_buffer_allocator.clone(),
            state.ctx.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .context("Failed to create upload command buffer")?;

        state.avatar_renderer.upload_atlas(&mut upload_builder)?;

        let upload_cb = upload_builder.build().context("Failed to build upload command buffer")?;
        let upload_future = state.previous_frame_end.take().unwrap()
            .then_execute(state.ctx.queue.clone(), upload_cb)
            .context("Failed to execute upload")?
            .then_signal_fence_and_flush()
            .map_err(Validated::unwrap)
            .context("Failed to flush upload")?;
        upload_future.wait(None).context("Failed to wait for upload")?;
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
                clear_values: vec![Some(theme::BACKGROUND.to_array().into()), None],
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

/// Draw the UI output into a command buffer builder (shared by all render paths).
fn render_output_to_builder(
    builder: &mut AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
    state: &RenderState,
    output: WidgetOutput,
    viewport: Viewport,
) -> Result<()> {
    if !output.spline_vertices.is_empty() {
        let spline_buffer = state.spline_renderer.create_vertex_buffer(output.spline_vertices)?;
        state.spline_renderer.draw(builder, spline_buffer, viewport.clone())?;
    }
    if !output.avatar_vertices.is_empty() {
        let avatar_buffer = state.avatar_renderer.create_vertex_buffer(output.avatar_vertices)?;
        state.avatar_renderer.draw(builder, avatar_buffer, viewport.clone())?;
    }
    if !output.text_vertices.is_empty() {
        let vertex_buffer = state.text_renderer.create_vertex_buffer(output.text_vertices)?;
        state.text_renderer.draw(builder, vertex_buffer, viewport.clone())?;
    }
    if !output.bold_text_vertices.is_empty() {
        let bold_buffer = state.bold_text_renderer.create_vertex_buffer(output.bold_text_vertices)?;
        state.bold_text_renderer.draw(builder, bold_buffer, viewport)?;
    }
    Ok(())
}

fn capture_screenshot(app: &mut App) -> Result<image::RgbaImage> {
    let state = app.state.as_mut().unwrap();
    state.previous_frame_end.as_mut().unwrap().cleanup_finished();

    let extent = state.surface.extent();
    let scale_factor = state.scale_factor;
    let (sidebar_ratio, graph_ratio) = (app.sidebar_ratio, app.graph_ratio);
    let elapsed = app.app_start.elapsed().as_secs_f32();
    let (graph_output, chrome_output, overlay_output) = build_ui_output(
        &mut app.tabs, app.active_tab, &app.tab_bar,
        &mut app.toast_manager, &app.repo_dialog, &app.settings_dialog, &app.confirm_dialog, &app.branch_name_dialog, &app.remote_dialog,
        &state.text_renderer, &state.bold_text_renderer, scale_factor, extent,
        &mut state.avatar_cache, &state.avatar_renderer,
        sidebar_ratio, graph_ratio,
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

    // Upload avatar atlas in a separate command buffer if dirty
    if state.avatar_renderer.needs_upload() {
        let mut upload_builder = AutoCommandBufferBuilder::primary(
            state.ctx.command_buffer_allocator.clone(),
            state.ctx.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .context("Failed to create upload command buffer")?;

        state.avatar_renderer.upload_atlas(&mut upload_builder)?;

        let upload_cb = upload_builder.build().context("Failed to build upload command buffer")?;
        let upload_future = state.previous_frame_end.take().unwrap()
            .then_execute(state.ctx.queue.clone(), upload_cb)
            .context("Failed to execute upload")?
            .then_signal_fence_and_flush()
            .map_err(Validated::unwrap)
            .context("Failed to flush upload")?;
        upload_future.wait(None).context("Failed to wait for upload")?;
        state.previous_frame_end = Some(sync::now(state.ctx.device.clone()).boxed());
    }

    // Acquire image
    let (image_index, _, acquire_future) = acquire_next_image(state.surface.swapchain.clone(), None)
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
                clear_values: vec![Some(theme::BACKGROUND.to_array().into()), None],
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
    state.previous_frame_end.as_mut().unwrap().cleanup_finished();

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
        &mut app.tabs, app.active_tab, &app.tab_bar,
        &mut app.toast_manager, &app.repo_dialog, &app.settings_dialog, &app.confirm_dialog, &app.branch_name_dialog, &app.remote_dialog,
        &state.text_renderer, &state.bold_text_renderer, scale_factor, extent,
        &mut state.avatar_cache, &state.avatar_renderer,
        sidebar_ratio, graph_ratio,
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

    // Upload avatar atlas in a separate command buffer if dirty
    if state.avatar_renderer.needs_upload() {
        let mut upload_builder = AutoCommandBufferBuilder::primary(
            state.ctx.command_buffer_allocator.clone(),
            state.ctx.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .context("Failed to create upload command buffer")?;

        state.avatar_renderer.upload_atlas(&mut upload_builder)?;

        let upload_cb = upload_builder.build().context("Failed to build upload command buffer")?;
        let upload_future = state.previous_frame_end.take().unwrap()
            .then_execute(state.ctx.queue.clone(), upload_cb)
            .context("Failed to execute upload")?
            .then_signal_fence_and_flush()
            .map_err(Validated::unwrap)
            .context("Failed to flush upload")?;
        upload_future.wait(None).context("Failed to wait for upload")?;
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
                clear_values: vec![Some(theme::BACKGROUND.to_array().into()), None],
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
    let Some(ref state_str) = app.cli_args.screenshot_state else { return };

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
                if let Some(ref repo) = repo_tab.repo
                    && let Ok(info) = repo.full_commit_info(oid)
                {
                    let diff_files = repo.diff_for_commit(oid).unwrap_or_default();
                    let sm_entries = repo.submodules_at_commit(oid).unwrap_or_default();
                    view_state.commit_detail_view.set_commit(info, diff_files.clone(), sm_entries);
                    if let Some(first_file) = diff_files.first() {
                        let title = first_file.path.clone();
                        view_state.diff_view.set_diff(vec![first_file.clone()], title);
                    }
                }
            }
        }
        other => {
            eprintln!("Unknown screenshot state: '{}'. Valid states: open-dialog, search, context-menu, commit-detail", other);
        }
    }
}
