mod config;
mod git;
mod input;
mod renderer;
mod ui;
mod views;

use anyhow::{Context, Result};
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
use crate::git::{CommitInfo, GitRepo, RemoteOpResult};
use crate::input::{InputEvent, InputState, Key};
use crate::renderer::{capture_to_buffer, OffscreenTarget, SurfaceManager, VulkanContext};
use crate::ui::{AvatarCache, AvatarRenderer, Rect, ScreenLayout, SplineRenderer, TextRenderer, Widget, WidgetOutput};
use crate::ui::widget::theme;
use crate::ui::widgets::{BranchNameDialog, BranchNameDialogAction, ConfirmDialog, ConfirmDialogAction, ContextMenu, MenuAction, MenuItem, HeaderBar, RepoDialog, RepoDialogAction, SettingsDialog, SettingsDialogAction, ShortcutBar, ShortcutContext, TabBar, TabAction, ToastManager, ToastSeverity};
use crate::views::{BranchSidebar, CommitDetailView, CommitDetailAction, CommitGraphView, GraphAction, DiffView, DiffAction, SecondaryReposView, StagingWell, StagingAction, SidebarAction};

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
    Staging,
    Sidebar,
}

/// Application-level messages for state changes
#[derive(Clone, Debug)]
enum AppMessage {
    StageFile(String),
    UnstageFile(String),
    StageAll,
    UnstageAll,
    Commit(String),
    Fetch,
    Pull,
    Push,
    SelectedCommit(Oid),
    ViewCommitFileDiff(Oid, String),
    ViewDiff(String, bool), // (path, staged)
    CheckoutBranch(String),
    CheckoutRemoteBranch(String, String),
    DeleteBranch(String),
    StageHunk(String, usize),    // (file_path, hunk_index)
    UnstageHunk(String, usize),  // (file_path, hunk_index)
    DiscardFile(String),
    LoadMoreCommits,
    DeleteSubmodule(String),
    UpdateSubmodule(String),
    JumpToWorktreeBranch(String),
    RemoveWorktree(String),
    MergeBranch(String),
    RebaseBranch(String),
    CreateBranch(String, Oid),  // (name, at_commit)
    StashPush,
    StashPop,
    StashApply(usize),
    StashDrop(usize),
    StashPopIndex(usize),
    CherryPick(Oid),
    AmendCommit(String),
    ToggleAmend,
}

/// Per-tab repository data
struct RepoTab {
    repo: Option<GitRepo>,
    commits: Vec<CommitInfo>,
    name: String,
    _path: PathBuf,
}

/// Per-tab UI view state
struct TabViewState {
    focused_panel: FocusedPanel,
    header_bar: HeaderBar,
    shortcut_bar: ShortcutBar,
    branch_sidebar: BranchSidebar,
    commit_graph_view: CommitGraphView,
    staging_well: StagingWell,
    secondary_repos_view: SecondaryReposView,
    diff_view: DiffView,
    commit_detail_view: CommitDetailView,
    context_menu: ContextMenu,
    /// Oid of the commit that was right-clicked for context menu
    context_menu_commit: Option<Oid>,
    last_diff_commit: Option<Oid>,
    pending_messages: Vec<AppMessage>,
    fetch_receiver: Option<Receiver<RemoteOpResult>>,
    pull_receiver: Option<Receiver<RemoteOpResult>>,
    push_receiver: Option<Receiver<RemoteOpResult>>,
    /// Generic async receiver for submodule/worktree ops (label for toast)
    generic_op_receiver: Option<(Receiver<RemoteOpResult>, String)>,
}

impl TabViewState {
    fn new() -> Self {
        Self {
            focused_panel: FocusedPanel::Graph,
            header_bar: HeaderBar::new(),
            shortcut_bar: ShortcutBar::new(),
            branch_sidebar: BranchSidebar::new(),
            commit_graph_view: CommitGraphView::new(),
            staging_well: StagingWell::new(),
            secondary_repos_view: SecondaryReposView::new(),
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
    /// Horizontal divider between staging and right panel (diff/detail)
    StagingRight,
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
    pending_confirm_action: Option<AppMessage>,
    toast_manager: ToastManager,
    state: Option<RenderState>,
    /// Which divider is currently being dragged, if any
    divider_drag: Option<DividerDrag>,
    /// Fraction of total width for sidebar (default ~0.14)
    sidebar_ratio: f32,
    /// Fraction of content width (after sidebar) for graph (default 0.55)
    graph_ratio: f32,
    /// Fraction of right panel height for staging (default 0.45)
    staging_ratio: f32,
    /// Whether the shortcut bar is visible
    shortcut_bar_visible: bool,
}

/// Initialized render state (after window creation) - shared across all tabs
struct RenderState {
    window: Arc<Window>,
    ctx: VulkanContext,
    surface: SurfaceManager,
    text_renderer: TextRenderer,
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
                    let commits = repo.commit_graph(MAX_COMMITS).unwrap_or_default();
                    let name = repo.repo_name();
                    let location: String = repo.workdir()
                        .map(|p| format!("{:?}", p))
                        .unwrap_or_else(|| format!("{:?} (bare)", repo.repo_name()));
                    println!("Loaded {} commits from {}", commits.len(), location);

                    tab_bar.add_tab(name.clone());
                    tabs.push((
                        RepoTab {
                            repo: Some(repo),
                            commits,
                            name,
                            _path: repo_path.clone(),
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
                            _path: repo_path.clone(),
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
            pending_confirm_action: None,
            toast_manager: ToastManager::new(),
            state: None,
            divider_drag: None,
            sidebar_ratio: 0.14,
            graph_ratio: 0.55,
            staging_ratio: 0.45,
            shortcut_bar_visible,
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

        // Create render pass
        let render_pass = vulkano::single_pass_renderpass!(
            ctx.device.clone(),
            attachments: {
                color: {
                    format: ctx.device.physical_device()
                        .surface_formats(&surface, Default::default())
                        .unwrap()[0].0,
                    samples: 1,
                    load_op: Clear,
                    store_op: Store,
                },
            },
            pass: {
                color: [color],
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
        for (repo_tab, view_state) in &mut self.tabs {
            view_state.commit_graph_view.row_scale = row_scale;
            init_tab_view(repo_tab, view_state, &text_renderer, scale);
        }

        self.state = Some(RenderState {
            window,
            ctx,
            surface: surface_mgr,
            text_renderer,
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

            // Update working directory status
            if let Ok(status) = repo.status() {
                view_state.commit_graph_view.working_dir_status = Some(status.clone());
                view_state.staging_well.update_status(&status);
                view_state.header_bar.has_staged = !status.staged.is_empty();
            }

            // Update ahead/behind
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

        if repo_tab.repo.is_none() {
            return;
        }

        // Helper macro to borrow repo immutably within a match arm.
        // Each arm re-borrows so the immutable borrow ends before the next arm
        // that might need a mutable borrow of repo_tab.
        macro_rules! repo {
            () => {
                repo_tab.repo.as_ref().unwrap()
            };
        }

        for msg in messages {
            match msg {
                AppMessage::StageFile(path) => {
                    if let Err(e) = repo!().stage_file(&path) {
                        eprintln!("Failed to stage {}: {}", path, e);
                        self.toast_manager.push(
                            format!("Stage failed: {}", e),
                            ToastSeverity::Error,
                        );
                    }
                }
                AppMessage::UnstageFile(path) => {
                    if let Err(e) = repo!().unstage_file(&path) {
                        eprintln!("Failed to unstage {}: {}", path, e);
                        self.toast_manager.push(
                            format!("Unstage failed: {}", e),
                            ToastSeverity::Error,
                        );
                    }
                }
                AppMessage::StageAll => {
                    if let Ok(status) = repo!().status() {
                        for file in &status.unstaged {
                            let _ = repo!().stage_file(&file.path);
                        }
                    }
                }
                AppMessage::UnstageAll => {
                    if let Ok(status) = repo!().status() {
                        for file in &status.staged {
                            let _ = repo!().unstage_file(&file.path);
                        }
                    }
                }
                AppMessage::Commit(message) => {
                    match repo!().commit(&message) {
                        Ok(oid) => {
                            println!("Created commit: {}", oid);
                            refresh_repo_state(repo_tab, view_state);
                            view_state.staging_well.clear_message();
                            self.toast_manager.push(
                                format!("Commit {}", &oid.to_string()[..7]),
                                ToastSeverity::Success,
                            );
                        }
                        Err(e) => {
                            eprintln!("Failed to commit: {}", e);
                            self.toast_manager.push(
                                format!("Commit failed: {}", e),
                                ToastSeverity::Error,
                            );
                        }
                    }
                }
                AppMessage::Fetch => {
                    if view_state.fetch_receiver.is_some() {
                        eprintln!("Fetch already in progress");
                        continue;
                    }
                    if let Some(workdir) = repo!().working_dir_path() {
                        let remote = repo!().default_remote().unwrap_or_else(|_| "origin".to_string());
                        println!("Fetching from {}...", remote);
                        let rx = crate::git::fetch_remote_async(workdir, remote);
                        view_state.fetch_receiver = Some(rx);
                        view_state.header_bar.fetching = true;
                    } else {
                        eprintln!("No working directory for fetch");
                    }
                }
                AppMessage::Pull => {
                    if view_state.pull_receiver.is_some() {
                        eprintln!("Pull already in progress");
                        continue;
                    }
                    if let Some(workdir) = repo!().working_dir_path() {
                        let remote = repo!().default_remote().unwrap_or_else(|_| "origin".to_string());
                        let branch = repo!().current_branch().unwrap_or_else(|_| "HEAD".to_string());
                        println!("Pulling {} from {}...", branch, remote);
                        let rx = crate::git::pull_remote_async(workdir, remote, branch);
                        view_state.pull_receiver = Some(rx);
                        view_state.header_bar.pulling = true;
                    } else {
                        eprintln!("No working directory for pull");
                    }
                }
                AppMessage::Push => {
                    if view_state.push_receiver.is_some() {
                        eprintln!("Push already in progress");
                        continue;
                    }
                    if let Some(workdir) = repo!().working_dir_path() {
                        let remote = repo!().default_remote().unwrap_or_else(|_| "origin".to_string());
                        let branch = repo!().current_branch().unwrap_or_else(|_| "HEAD".to_string());
                        println!("Pushing {} to {}...", branch, remote);
                        let rx = crate::git::push_remote_async(workdir, remote, branch);
                        view_state.push_receiver = Some(rx);
                        view_state.header_bar.pushing = true;
                    } else {
                        eprintln!("No working directory for push");
                    }
                }
                AppMessage::SelectedCommit(oid) => {
                    let full_info = repo!().full_commit_info(oid);
                    match repo!().diff_for_commit(oid) {
                        Ok(diff_files) => {
                            if let Ok(info) = full_info {
                                view_state.commit_detail_view.set_commit(info, diff_files.clone());
                            }
                            if let Some(first_file) = diff_files.first() {
                                let title = first_file.path.clone();
                                view_state.diff_view.set_diff(vec![first_file.clone()], title);
                            } else {
                                let title = repo_tab.commits.iter()
                                    .find(|c| c.id == oid)
                                    .map(|c| format!("{} {}", c.short_id, c.summary))
                                    .unwrap_or_else(|| oid.to_string());
                                view_state.diff_view.set_diff(diff_files, title);
                            }
                            view_state.last_diff_commit = Some(oid);
                        }
                        Err(e) => {
                            eprintln!("Failed to load diff for {}: {}", oid, e);
                        }
                    }
                }
                AppMessage::ViewCommitFileDiff(oid, path) => {
                    match repo!().diff_file_in_commit(oid, &path) {
                        Ok(diff_files) => {
                            view_state.diff_view.set_diff(diff_files, path);
                        }
                        Err(e) => {
                            eprintln!("Failed to load diff for file '{}': {}", path, e);
                        }
                    }
                }
                AppMessage::ViewDiff(path, staged) => {
                    match repo!().diff_working_file(&path, staged) {
                        Ok(hunks) => {
                            let diff_file = crate::git::DiffFile::from_hunks(path.clone(), hunks);
                            let title = if staged {
                                format!("Staged: {}", path)
                            } else {
                                format!("Unstaged: {}", path)
                            };
                            if staged {
                                view_state.diff_view.set_staged_diff(vec![diff_file], title);
                            } else {
                                view_state.diff_view.set_diff(vec![diff_file], title);
                            }
                            view_state.last_diff_commit = None;
                        }
                        Err(e) => {
                            eprintln!("Failed to load diff for {}: {}", path, e);
                        }
                    }
                }
                AppMessage::CheckoutBranch(name) => {
                    match repo!().checkout_branch(&name) {
                        Ok(()) => {
                            println!("Checked out branch: {}", name);
                            refresh_repo_state(repo_tab, view_state);
                            self.toast_manager.push(
                                format!("Switched to {}", name),
                                ToastSeverity::Success,
                            );
                        }
                        Err(e) => {
                            eprintln!("Failed to checkout branch '{}': {}", name, e);
                            self.toast_manager.push(
                                format!("Checkout failed: {}", e),
                                ToastSeverity::Error,
                            );
                        }
                    }
                }
                AppMessage::CheckoutRemoteBranch(remote, branch) => {
                    match repo!().checkout_remote_branch(&remote, &branch) {
                        Ok(()) => {
                            println!("Checked out remote branch: {}/{}", remote, branch);
                            refresh_repo_state(repo_tab, view_state);
                            self.toast_manager.push(
                                format!("Switched to {}/{}", remote, branch),
                                ToastSeverity::Success,
                            );
                        }
                        Err(e) => {
                            eprintln!("Failed to checkout remote branch '{}/{}': {}", remote, branch, e);
                            self.toast_manager.push(
                                format!("Checkout failed: {}", e),
                                ToastSeverity::Error,
                            );
                        }
                    }
                }
                AppMessage::DeleteBranch(name) => {
                    match repo!().delete_branch(&name) {
                        Ok(()) => {
                            println!("Deleted branch: {}", name);
                            refresh_repo_state(repo_tab, view_state);
                            self.toast_manager.push(
                                format!("Deleted branch {}", name),
                                ToastSeverity::Success,
                            );
                        }
                        Err(e) => {
                            eprintln!("Failed to delete branch '{}': {}", name, e);
                            // Show root cause for a cleaner message
                            let root = e.root_cause().to_string();
                            self.toast_manager.push(
                                format!("Cannot delete '{}': {}", name, root),
                                ToastSeverity::Error,
                            );
                        }
                    }
                }
                AppMessage::StageHunk(path, hunk_idx) => {
                    match repo!().stage_hunk(&path, hunk_idx) {
                        Ok(()) => {
                            self.toast_manager.push(
                                format!("Staged hunk {} in {}", hunk_idx + 1, path),
                                ToastSeverity::Success,
                            );
                            if let Ok(hunks) = repo!().diff_working_file(&path, false) {
                                if hunks.is_empty() {
                                    view_state.diff_view.clear();
                                } else {
                                    let diff_file = crate::git::DiffFile::from_hunks(path.clone(), hunks);
                                    view_state.diff_view.set_diff(vec![diff_file], path);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to stage hunk: {}", e);
                            self.toast_manager.push(
                                format!("Stage hunk failed: {}", e),
                                ToastSeverity::Error,
                            );
                        }
                    }
                }
                AppMessage::UnstageHunk(path, hunk_idx) => {
                    match repo!().unstage_hunk(&path, hunk_idx) {
                        Ok(()) => {
                            self.toast_manager.push(
                                format!("Unstaged hunk {} in {}", hunk_idx + 1, path),
                                ToastSeverity::Success,
                            );
                            if let Ok(hunks) = repo!().diff_working_file(&path, true) {
                                if hunks.is_empty() {
                                    view_state.diff_view.clear();
                                } else {
                                    let diff_file = crate::git::DiffFile::from_hunks(path.clone(), hunks);
                                    view_state.diff_view.set_staged_diff(vec![diff_file], path);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to unstage hunk: {}", e);
                            self.toast_manager.push(
                                format!("Unstage hunk failed: {}", e),
                                ToastSeverity::Error,
                            );
                        }
                    }
                }
                AppMessage::DiscardFile(path) => {
                    match repo!().discard_file(&path) {
                        Ok(()) => {
                            self.toast_manager.push(
                                format!("Discarded: {}", path),
                                ToastSeverity::Info,
                            );
                        }
                        Err(e) => {
                            self.toast_manager.push(
                                format!("Discard failed: {}", e),
                                ToastSeverity::Error,
                            );
                        }
                    }
                }
                AppMessage::LoadMoreCommits => {
                    let current_count = repo_tab.commits.len();
                    let new_count = current_count + 50;
                    if let Ok(commits) = repo!().commit_graph(new_count) {
                        repo_tab.commits = commits;
                        view_state.commit_graph_view.update_layout(&repo_tab.commits);
                    }
                    view_state.commit_graph_view.finish_loading();
                }
                AppMessage::DeleteSubmodule(name) => {
                    if view_state.generic_op_receiver.is_some() {
                        self.toast_manager.push("Another operation is in progress".to_string(), ToastSeverity::Info);
                        continue;
                    }
                    if let Some(workdir) = repo!().working_dir_path() {
                        let rx = crate::git::remove_submodule_async(workdir, name.clone());
                        view_state.generic_op_receiver = Some((rx, format!("Delete submodule '{}'", name)));
                        self.toast_manager.push(format!("Removing submodule '{}'...", name), ToastSeverity::Info);
                    }
                }
                AppMessage::UpdateSubmodule(name) => {
                    if view_state.generic_op_receiver.is_some() {
                        self.toast_manager.push("Another operation is in progress".to_string(), ToastSeverity::Info);
                        continue;
                    }
                    if let Some(workdir) = repo!().working_dir_path() {
                        let rx = crate::git::update_submodule_async(workdir, name.clone());
                        view_state.generic_op_receiver = Some((rx, format!("Update submodule '{}'", name)));
                        self.toast_manager.push(format!("Updating submodule '{}'...", name), ToastSeverity::Info);
                    }
                }
                AppMessage::JumpToWorktreeBranch(name) => {
                    // Find the worktree by name, get its branch, find the branch tip, select it
                    if let Some(wt) = view_state.branch_sidebar.worktrees.iter().find(|w| w.name == name) {
                        let branch_name = wt.branch.clone();
                        if let Some(tip) = view_state.commit_graph_view.branch_tips.iter()
                            .find(|t| t.name == branch_name && !t.is_remote) {
                                view_state.commit_graph_view.selected_commit = Some(tip.oid);
                                let graph_bounds = if let Some(ref state) = self.state {
                                    let extent = state.surface.extent();
                                    let scale = state.scale_factor as f32;
                                    let screen_bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
                                    let tab_bar_height = if tab_count > 1 { TabBar::height(scale) } else { 0.0 };
                                    let (_tab_bar_bounds, main_bounds) = screen_bounds.take_top(tab_bar_height);
                                    let layout = ScreenLayout::compute_with_ratios_and_shortcut(
                                        main_bounds, 4.0, scale,
                                        Some(self.sidebar_ratio),
                                        Some(self.graph_ratio),
                                        Some(self.staging_ratio),
                                        self.shortcut_bar_visible,
                                    );
                                    layout.graph
                                } else {
                                    Rect::new(0.0, 0.0, 1920.0, 1080.0)
                                };
                                view_state.commit_graph_view.scroll_to_selection(&repo_tab.commits, graph_bounds);
                                self.toast_manager.push(format!("Jumped to branch '{}'", branch_name), ToastSeverity::Info);
                        } else {
                            self.toast_manager.push(format!("Branch '{}' not found in graph", branch_name), ToastSeverity::Error);
                        }
                    } else {
                        self.toast_manager.push(format!("Worktree '{}' not found", name), ToastSeverity::Error);
                    }
                }
                AppMessage::RemoveWorktree(name) => {
                    if view_state.generic_op_receiver.is_some() {
                        self.toast_manager.push("Another operation is in progress".to_string(), ToastSeverity::Info);
                        continue;
                    }
                    if let Some(workdir) = repo!().working_dir_path() {
                        let rx = crate::git::remove_worktree_async(workdir, name.clone());
                        view_state.generic_op_receiver = Some((rx, format!("Remove worktree '{}'", name)));
                        self.toast_manager.push(format!("Removing worktree '{}'...", name), ToastSeverity::Info);
                    }
                }
                AppMessage::MergeBranch(name) => {
                    if view_state.generic_op_receiver.is_some() {
                        self.toast_manager.push("Another operation is in progress".to_string(), ToastSeverity::Info);
                        continue;
                    }
                    if let Some(workdir) = repo!().working_dir_path() {
                        let rx = crate::git::merge_branch_async(workdir, name.clone());
                        view_state.generic_op_receiver = Some((rx, format!("Merge '{}'", name)));
                        self.toast_manager.push(format!("Merging '{}'...", name), ToastSeverity::Info);
                    }
                }
                AppMessage::RebaseBranch(name) => {
                    if view_state.generic_op_receiver.is_some() {
                        self.toast_manager.push("Another operation is in progress".to_string(), ToastSeverity::Info);
                        continue;
                    }
                    if let Some(workdir) = repo!().working_dir_path() {
                        let rx = crate::git::rebase_branch_async(workdir, name.clone());
                        view_state.generic_op_receiver = Some((rx, format!("Rebase onto '{}'", name)));
                        self.toast_manager.push(format!("Rebasing onto '{}'...", name), ToastSeverity::Info);
                    }
                }
                AppMessage::CreateBranch(name, oid) => {
                    match repo!().create_branch_at(&name, oid) {
                        Ok(()) => {
                            refresh_repo_state(repo_tab, view_state);
                            self.toast_manager.push(
                                format!("Created branch '{}'", name),
                                ToastSeverity::Success,
                            );
                        }
                        Err(e) => {
                            self.toast_manager.push(
                                format!("Create branch failed: {}", e),
                                ToastSeverity::Error,
                            );
                        }
                    }
                }
                AppMessage::StashPush => {
                    if view_state.generic_op_receiver.is_some() {
                        self.toast_manager.push("Another operation is in progress".to_string(), ToastSeverity::Info);
                        continue;
                    }
                    if let Some(workdir) = repo!().working_dir_path() {
                        let rx = crate::git::stash_push_async(workdir);
                        view_state.generic_op_receiver = Some((rx, "Stash push".to_string()));
                        self.toast_manager.push("Stashing changes...".to_string(), ToastSeverity::Info);
                    }
                }
                AppMessage::StashPop => {
                    if view_state.generic_op_receiver.is_some() {
                        self.toast_manager.push("Another operation is in progress".to_string(), ToastSeverity::Info);
                        continue;
                    }
                    if let Some(workdir) = repo!().working_dir_path() {
                        let rx = crate::git::stash_pop_async(workdir);
                        view_state.generic_op_receiver = Some((rx, "Stash pop".to_string()));
                        self.toast_manager.push("Popping stash...".to_string(), ToastSeverity::Info);
                    }
                }
                AppMessage::StashApply(index) => {
                    if view_state.generic_op_receiver.is_some() {
                        self.toast_manager.push("Another operation is in progress".to_string(), ToastSeverity::Info);
                        continue;
                    }
                    if let Some(workdir) = repo!().working_dir_path() {
                        let rx = crate::git::stash_apply_async(workdir, index);
                        view_state.generic_op_receiver = Some((rx, format!("Stash apply @{{{}}}", index)));
                        self.toast_manager.push(format!("Applying stash@{{{}}}...", index), ToastSeverity::Info);
                    }
                }
                AppMessage::StashDrop(index) => {
                    if view_state.generic_op_receiver.is_some() {
                        self.toast_manager.push("Another operation is in progress".to_string(), ToastSeverity::Info);
                        continue;
                    }
                    if let Some(workdir) = repo!().working_dir_path() {
                        let rx = crate::git::stash_drop_async(workdir, index);
                        view_state.generic_op_receiver = Some((rx, format!("Stash drop @{{{}}}", index)));
                        self.toast_manager.push(format!("Dropping stash@{{{}}}...", index), ToastSeverity::Info);
                    }
                }
                AppMessage::StashPopIndex(index) => {
                    if view_state.generic_op_receiver.is_some() {
                        self.toast_manager.push("Another operation is in progress".to_string(), ToastSeverity::Info);
                        continue;
                    }
                    if let Some(workdir) = repo!().working_dir_path() {
                        let rx = crate::git::stash_pop_index_async(workdir, index);
                        view_state.generic_op_receiver = Some((rx, format!("Stash pop @{{{}}}", index)));
                        self.toast_manager.push(format!("Popping stash@{{{}}}...", index), ToastSeverity::Info);
                    }
                }
                AppMessage::CherryPick(oid) => {
                    if view_state.generic_op_receiver.is_some() {
                        self.toast_manager.push("Another operation is in progress".to_string(), ToastSeverity::Info);
                        continue;
                    }
                    if let Some(workdir) = repo!().working_dir_path() {
                        let sha = oid.to_string();
                        let rx = crate::git::cherry_pick_async(workdir, sha.clone());
                        view_state.generic_op_receiver = Some((rx, format!("Cherry-pick {}", &sha[..7])));
                        self.toast_manager.push(format!("Cherry-picking {}...", &sha[..7]), ToastSeverity::Info);
                    }
                }
                AppMessage::AmendCommit(message) => {
                    match repo!().amend_commit(&message) {
                        Ok(oid) => {
                            println!("Amended commit: {}", oid);
                            refresh_repo_state(repo_tab, view_state);
                            view_state.staging_well.exit_amend_mode();
                            self.toast_manager.push(
                                format!("Amended {}", &oid.to_string()[..7]),
                                ToastSeverity::Success,
                            );
                        }
                        Err(e) => {
                            eprintln!("Failed to amend: {}", e);
                            self.toast_manager.push(
                                format!("Amend failed: {}", e),
                                ToastSeverity::Error,
                            );
                        }
                    }
                }
                AppMessage::ToggleAmend => {
                    if view_state.staging_well.amend_mode {
                        view_state.staging_well.exit_amend_mode();
                    } else if let Some((subject, body)) = repo!().head_commit_message() {
                        view_state.staging_well.enter_amend_mode(&subject, &body);
                    } else {
                        self.toast_manager.push(
                            "No HEAD commit to amend".to_string(),
                            ToastSeverity::Error,
                        );
                    }
                }
            }
        }

        // Refresh status after processing all messages
        self.refresh_status();
    }

    fn poll_remote_ops(&mut self) {
        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else { return };

        // Poll fetch
        if let Some(ref rx) = view_state.fetch_receiver
            && let Ok(result) = rx.try_recv() {
                view_state.header_bar.fetching = false;
                view_state.fetch_receiver = None;
                if result.success {
                    println!("Fetch completed successfully");
                    if !result.error.is_empty() {
                        println!("{}", result.error.trim());
                    }
                    self.toast_manager.push("Fetch complete", ToastSeverity::Success);
                    refresh_repo_state(repo_tab, view_state);
                } else {
                    eprintln!("Fetch failed: {}", result.error);
                    self.toast_manager.push(
                        format!("Fetch failed: {}", result.error.lines().next().unwrap_or("unknown error")),
                        ToastSeverity::Error,
                    );
                }
            }

        // Poll pull
        if let Some(ref rx) = view_state.pull_receiver
            && let Ok(result) = rx.try_recv() {
                view_state.header_bar.pulling = false;
                view_state.pull_receiver = None;
                if result.success {
                    println!("Pull completed successfully");
                    if !result.error.is_empty() {
                        println!("{}", result.error.trim());
                    }
                    self.toast_manager.push("Pull complete", ToastSeverity::Success);
                    refresh_repo_state(repo_tab, view_state);
                } else {
                    eprintln!("Pull failed: {}", result.error);
                    self.toast_manager.push(
                        format!("Pull failed: {}", result.error.lines().next().unwrap_or("unknown error")),
                        ToastSeverity::Error,
                    );
                }
            }

        // Poll push (also does full refresh to update ahead/behind and branch state)
        if let Some(ref rx) = view_state.push_receiver
            && let Ok(result) = rx.try_recv() {
                view_state.header_bar.pushing = false;
                view_state.push_receiver = None;
                if result.success {
                    println!("Push completed successfully");
                    if !result.error.is_empty() {
                        println!("{}", result.error.trim());
                    }
                    self.toast_manager.push("Push complete", ToastSeverity::Success);
                    refresh_repo_state(repo_tab, view_state);
                } else {
                    eprintln!("Push failed: {}", result.error);
                    self.toast_manager.push(
                        format!("Push failed: {}", result.error.lines().next().unwrap_or("unknown error")),
                        ToastSeverity::Error,
                    );
                }
            }

        // Poll generic async ops (submodule/worktree operations)
        if let Some((ref rx, ref label)) = view_state.generic_op_receiver
            && let Ok(result) = rx.try_recv() {
                let label = label.clone();
                view_state.generic_op_receiver = None;
                if result.success {
                    self.toast_manager.push(format!("{} complete", label), ToastSeverity::Success);
                    refresh_repo_state(repo_tab, view_state);
                    // Also refresh submodules/worktrees/stashes
                    if let Some(ref repo) = repo_tab.repo {
                        if let Ok(submodules) = repo.submodules() {
                            view_state.secondary_repos_view.set_submodules(submodules.clone());
                            view_state.branch_sidebar.submodules = submodules;
                        }
                        if let Ok(worktrees) = repo.worktrees() {
                            view_state.secondary_repos_view.set_worktrees(worktrees.clone());
                            view_state.branch_sidebar.worktrees = worktrees;
                        }
                        view_state.branch_sidebar.stashes = repo.stash_list();
                    }
                } else {
                    self.toast_manager.push(
                        format!("{} failed: {}", label, result.error.lines().next().unwrap_or("unknown error")),
                        ToastSeverity::Error,
                    );
                }
            }
    }

    /// Open a new repo and add it as a tab
    fn open_repo_tab(&mut self, path: PathBuf) {
        match GitRepo::open(&path) {
            Ok(repo) => {
                let commits = repo.commit_graph(MAX_COMMITS).unwrap_or_default();
                let name = repo.repo_name();
                println!("Opened {} with {} commits", name, commits.len());

                self.tab_bar.add_tab(name.clone());
                let mut view_state = TabViewState::new();

                // Initialize the view if render state exists
                let mut repo_tab = RepoTab {
                    repo: Some(repo),
                    commits,
                    name,
                    _path: path,
                };

                if let Some(ref render_state) = self.state {
                    view_state.commit_graph_view.row_scale = self.settings_dialog.row_scale;
                    init_tab_view(&mut repo_tab, &mut view_state, &render_state.text_renderer, render_state.scale_factor as f32);
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
                eprintln!("Failed to open repo at {:?}: {}", path, e);
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
}

/// Refresh commits, branch tips, tags, and header info from the repo.
/// Call this after any operation that changes branches, commits, or remote state.
fn refresh_repo_state(repo_tab: &mut RepoTab, view_state: &mut TabViewState) {
    let Some(ref repo) = repo_tab.repo else { return };

    repo_tab.commits = repo.commit_graph(MAX_COMMITS).unwrap_or_default();
    view_state.commit_graph_view.update_layout(&repo_tab.commits);
    view_state.commit_graph_view.head_oid = repo.head_oid().ok();

    let branch_tips = repo.branch_tips().unwrap_or_default();
    let tags = repo.tags().unwrap_or_default();
    let current = repo.current_branch().unwrap_or_default();

    view_state.commit_graph_view.branch_tips = branch_tips.clone();
    view_state.commit_graph_view.tags = tags.clone();
    view_state.branch_sidebar.set_branch_data(&branch_tips, &tags, current.clone());

    let (ahead, behind) = repo.ahead_behind().unwrap_or((0, 0));
    view_state.header_bar.set_repo_info(
        view_state.header_bar.repo_name.clone(),
        current,
        ahead,
        behind,
    );
}

/// Initialize a tab's view state from its repo data
fn init_tab_view(repo_tab: &mut RepoTab, view_state: &mut TabViewState, text_renderer: &TextRenderer, scale: f32) {
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

        // Set working dir status on graph view
        view_state.commit_graph_view.working_dir_status = repo.status().ok();

        // Set staging status
        view_state.header_bar.has_staged = repo.status()
            .map(|s| !s.staged.is_empty())
            .unwrap_or(false);

        // Load submodules and worktrees
        if let Ok(submodules) = repo.submodules() {
            view_state.secondary_repos_view.set_submodules(submodules.clone());
            view_state.branch_sidebar.submodules = submodules;
        }
        if let Ok(worktrees) = repo.worktrees() {
            view_state.secondary_repos_view.set_worktrees(worktrees.clone());
            view_state.branch_sidebar.worktrees = worktrees;
        }

        // Load stashes
        view_state.branch_sidebar.stashes = repo.stash_list();
    }

    // Refresh commits, branches, tags, head, and header info
    refresh_repo_state(repo_tab, view_state);
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
                // Sync metrics on all tabs and recompute layouts
                for (repo_tab, view_state) in &mut self.tabs {
                    view_state.commit_graph_view.sync_metrics(&state.text_renderer);
                    view_state.commit_graph_view.update_layout(&repo_tab.commits);
                    view_state.branch_sidebar.sync_metrics(&state.text_renderer);
                    view_state.staging_well.set_scale(scale_factor as f32);
                }
                state.surface.needs_recreate = true;
            }

            WindowEvent::RedrawRequested => {
                // Poll background remote operations
                self.poll_remote_ops();
                // Process any pending messages
                self.process_messages();

                // Check for repo dialog actions
                if let Some(action) = self.repo_dialog.take_action() {
                    match action {
                        RepoDialogAction::Open(path) => {
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
                // Convert winit event to our InputEvent
                if let Some(input_event) = state.input_state.handle_window_event(win_event) {
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
                        Some(self.staging_ratio),
                        self.shortcut_bar_visible,
                    );

                    // Confirm dialog takes highest modal priority
                    if self.confirm_dialog.is_visible() {
                        self.confirm_dialog.handle_event(&input_event, screen_bounds);
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
                        return;
                    }

                    // Branch name dialog takes modal priority
                    if self.branch_name_dialog.is_visible() {
                        self.branch_name_dialog.handle_event(&input_event, screen_bounds);
                        if let Some(action) = self.branch_name_dialog.take_action() {
                            match action {
                                BranchNameDialogAction::Create(name, oid) => {
                                    if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                        view_state.pending_messages.push(AppMessage::CreateBranch(name, oid));
                                    }
                                }
                                BranchNameDialogAction::Cancel => {}
                            }
                        }
                        return;
                    }

                    // Settings dialog takes priority (modal)
                    if self.settings_dialog.is_visible() {
                        self.settings_dialog.handle_event(&input_event, screen_bounds);
                        if let Some(action) = self.settings_dialog.take_action() {
                            match action {
                                SettingsDialogAction::Close => {
                                    // Apply row scale to all graph views
                                    let row_scale = self.settings_dialog.row_scale;
                                    for (_, view_state) in &mut self.tabs {
                                        view_state.commit_graph_view.row_scale = row_scale;
                                        view_state.commit_graph_view.sync_metrics(&state.text_renderer);
                                    }
                                    // Persist settings to disk
                                    self.config.avatars_enabled = self.settings_dialog.show_avatars;
                                    self.config.fast_scroll = self.settings_dialog.scroll_speed >= 1.5;
                                    self.config.row_scale = self.settings_dialog.row_scale;
                                    self.config.save();
                                }
                            }
                        }
                        return;
                    }

                    // Dialog takes priority (modal)
                    if self.repo_dialog.is_visible() {
                        self.repo_dialog.handle_event(&input_event, screen_bounds);
                        return;
                    }

                    // Context menu takes priority when visible (overlay)
                    if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab)
                        && view_state.context_menu.is_visible() {
                            view_state.context_menu.handle_event(&input_event, screen_bounds);
                            if let Some(action) = view_state.context_menu.take_action() {
                                match action {
                                    MenuAction::Selected(action_id) => {
                                        handle_context_menu_action(
                                            &action_id,
                                            view_state,
                                            &mut self.toast_manager,
                                            &mut self.confirm_dialog,
                                            &mut self.branch_name_dialog,
                                            &mut self.pending_confirm_action,
                                        );
                                    }
                                }
                            }
                            return;
                        }

                    // ---- Divider drag handling ----
                    // Handle ongoing drag (MouseMove / MouseUp) before anything else
                    if self.divider_drag.is_some() {
                        match &input_event {
                            InputEvent::MouseMove { x, y, .. } => {
                                match self.divider_drag.unwrap() {
                                    DividerDrag::SidebarGraph => {
                                        // Convert mouse x to sidebar ratio of main_bounds width
                                        let ratio = (*x - main_bounds.x) / main_bounds.width;
                                        self.sidebar_ratio = ratio.clamp(0.05, 0.30);
                                    }
                                    DividerDrag::GraphRight => {
                                        // Content area starts after sidebar
                                        let sidebar_w = main_bounds.width * self.sidebar_ratio.clamp(0.05, 0.30);
                                        let content_x = main_bounds.x + sidebar_w;
                                        let content_w = main_bounds.width - sidebar_w;
                                        if content_w > 0.0 {
                                            let ratio = (*x - content_x) / content_w;
                                            self.graph_ratio = ratio.clamp(0.30, 0.80);
                                        }
                                    }
                                    DividerDrag::StagingRight => {
                                        // Staging ratio is fraction of right panel height
                                        // Right panel starts after header + shortcut bar
                                        let rp_y = layout.staging.y;
                                        let rp_h = layout.staging.height + layout.right_panel.height;
                                        if rp_h > 0.0 {
                                            let ratio = (*y - rp_y) / rp_h;
                                            self.staging_ratio = ratio.clamp(0.15, 0.85);
                                        }
                                    }
                                }
                                // Set appropriate cursor while dragging
                                if let Some(ref render_state) = self.state {
                                    let cursor = match self.divider_drag.unwrap() {
                                        DividerDrag::SidebarGraph | DividerDrag::GraphRight => CursorIcon::ColResize,
                                        DividerDrag::StagingRight => CursorIcon::RowResize,
                                    };
                                    render_state.window.set_cursor(cursor);
                                }
                                return;
                            }
                            InputEvent::MouseUp { .. } => {
                                self.divider_drag = None;
                                return;
                            }
                            _ => {}
                        }
                    }

                    // Start divider drag on MouseDown near divider edges
                    if let InputEvent::MouseDown { button: input::MouseButton::Left, x, y, .. } = &input_event {
                        let hit_tolerance = 5.0;

                        // Only check dividers in the main content area (below header)
                        if *y > layout.shortcut_bar.bottom() {
                            // Divider 1: sidebar | graph (vertical)
                            let sidebar_edge = layout.sidebar.right();
                            if (*x - sidebar_edge).abs() < hit_tolerance {
                                self.divider_drag = Some(DividerDrag::SidebarGraph);
                                return;
                            }

                            // Divider 2: graph | right panel (vertical)
                            let graph_edge = layout.graph.right();
                            if (*x - graph_edge).abs() < hit_tolerance {
                                self.divider_drag = Some(DividerDrag::GraphRight);
                                return;
                            }

                            // Divider 3: staging | right panel (horizontal, only in right column)
                            let staging_edge = layout.staging.bottom();
                            if (*y - staging_edge).abs() < hit_tolerance
                                && *x >= layout.staging.x
                                && *x <= layout.staging.right()
                            {
                                self.divider_drag = Some(DividerDrag::StagingRight);
                                return;
                            }
                        }
                    }

                    // Handle global keys first
                    if let InputEvent::KeyDown { key, modifiers, .. } = &input_event {
                        // Ctrl+O: open repo
                        if *key == Key::O && modifiers.only_ctrl() {
                            self.repo_dialog.show();
                            return;
                        }
                        // Ctrl+W: close tab
                        if *key == Key::W && modifiers.only_ctrl() {
                            if self.tabs.len() > 1 {
                                let idx = self.active_tab;
                                self.close_tab(idx);
                            }
                            return;
                        }
                        // Ctrl+Tab: next tab
                        if *key == Key::Tab && modifiers.only_ctrl() {
                            let next = (self.active_tab + 1) % self.tabs.len();
                            self.switch_tab(next);
                            return;
                        }
                        // Ctrl+Shift+Tab: previous tab
                        if *key == Key::Tab && modifiers.ctrl_shift() {
                            let prev = if self.active_tab == 0 {
                                self.tabs.len() - 1
                            } else {
                                self.active_tab - 1
                            };
                            self.switch_tab(prev);
                            return;
                        }
                        // Ctrl+S: stash push (only when staging text inputs are not focused)
                        if *key == Key::S && modifiers.only_ctrl() {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                if !view_state.staging_well.has_text_focus() {
                                    view_state.pending_messages.push(AppMessage::StashPush);
                                    return;
                                }
                            }
                        }
                        // Ctrl+Shift+S: stash pop
                        if *key == Key::S && modifiers.ctrl_shift() {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                view_state.pending_messages.push(AppMessage::StashPop);
                                return;
                            }
                        }
                        // Ctrl+Shift+A: toggle amend mode
                        if *key == Key::A && modifiers.ctrl_shift() {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                if !view_state.staging_well.has_text_focus() {
                                    view_state.pending_messages.push(AppMessage::ToggleAmend);
                                    return;
                                }
                            }
                        }
                    }

                    // Route to tab bar (if visible)
                    if self.tabs.len() > 1
                        && self.tab_bar.handle_event(&input_event, tab_bar_bounds).is_consumed() {
                            if let Some(action) = self.tab_bar.take_action() {
                                match action {
                                    TabAction::Select(idx) => self.switch_tab(idx),
                                    TabAction::Close(idx) => self.close_tab(idx),
                                    TabAction::New => self.repo_dialog.show(),
                                }
                            }
                            return;
                        }

                    // Route to active tab's views
                    let tab_count = self.tabs.len();
                    let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else { return };

                    // Handle per-tab global keys (except Tab, which is handled after panel routing)
                    if let InputEvent::KeyDown { key, .. } = &input_event
                        && key == &Key::Escape {
                            if view_state.diff_view.has_content() {
                                view_state.diff_view.clear();
                                view_state.last_diff_commit = None;
                            } else if view_state.commit_detail_view.has_content() {
                                view_state.commit_detail_view.clear();
                                view_state.last_diff_commit = None;
                            } else {
                                event_loop.exit();
                            }
                            return;
                        }

                    // Route to branch sidebar
                    if view_state.branch_sidebar.handle_event(&input_event, layout.sidebar).is_consumed() {
                        if let Some(action) = view_state.branch_sidebar.take_action() {
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
                                SidebarAction::DeleteSubmodule(name) => {
                                    self.confirm_dialog.show("Delete Submodule", &format!("Remove submodule '{}'? This will deinit and remove it.", name));
                                    self.pending_confirm_action = Some(AppMessage::DeleteSubmodule(name));
                                }
                                SidebarAction::UpdateSubmodule(name) => {
                                    view_state.pending_messages.push(AppMessage::UpdateSubmodule(name));
                                }
                                SidebarAction::OpenSubmoduleTerminal(name) => {
                                    self.toast_manager.push(format!("Open terminal for '{}' not yet implemented", name), ToastSeverity::Info);
                                }
                                SidebarAction::JumpToWorktreeBranch(name) => {
                                    view_state.pending_messages.push(AppMessage::JumpToWorktreeBranch(name));
                                }
                                SidebarAction::RemoveWorktree(name) => {
                                    self.confirm_dialog.show("Remove Worktree", &format!("Remove worktree '{}'?", name));
                                    self.pending_confirm_action = Some(AppMessage::RemoveWorktree(name));
                                }
                                SidebarAction::OpenWorktreeTerminal(name) => {
                                    self.toast_manager.push(format!("Open terminal for '{}' not yet implemented", name), ToastSeverity::Info);
                                }
                                SidebarAction::ApplyStash(index) => {
                                    view_state.pending_messages.push(AppMessage::StashApply(index));
                                }
                                SidebarAction::PopStash(index) => {
                                    view_state.pending_messages.push(AppMessage::StashPopIndex(index));
                                }
                                SidebarAction::DropStash(index) => {
                                    self.confirm_dialog.show("Drop Stash", &format!("Drop stash@{{{}}}? This cannot be undone.", index));
                                    self.pending_confirm_action = Some(AppMessage::StashDrop(index));
                                }
                            }
                        }
                        return;
                    }

                    // Route to header bar
                    if view_state.header_bar.handle_event(&input_event, layout.header).is_consumed() {
                        if let Some(action) = view_state.header_bar.take_action() {
                            use crate::ui::widgets::HeaderAction;
                            match action {
                                HeaderAction::Fetch => {
                                    view_state.pending_messages.push(AppMessage::Fetch);
                                }
                                HeaderAction::Pull => {
                                    view_state.pending_messages.push(AppMessage::Pull);
                                }
                                HeaderAction::Push => {
                                    view_state.pending_messages.push(AppMessage::Push);
                                }
                                HeaderAction::Commit => {
                                    view_state.focused_panel = FocusedPanel::Staging;
                                }
                                HeaderAction::Help => {
                                    self.shortcut_bar_visible = !self.shortcut_bar_visible;
                                    self.config.shortcut_bar_visible = self.shortcut_bar_visible;
                                    self.config.save();
                                }
                                HeaderAction::Settings => {
                                    self.settings_dialog.show();
                                }
                            }
                        }
                        return;
                    }

                    // Route to commit detail view when active
                    if view_state.commit_detail_view.has_content() {
                        let (detail_rect, _diff_rect) = layout.right_panel.split_vertical(0.40);
                        if view_state.commit_detail_view.handle_event(&input_event, detail_rect).is_consumed() {
                            if let Some(action) = view_state.commit_detail_view.take_action() {
                                match action {
                                    CommitDetailAction::ViewFileDiff(oid, path) => {
                                        view_state.pending_messages.push(AppMessage::ViewCommitFileDiff(oid, path));
                                    }
                                }
                            }
                            return;
                        }
                    }

                    // Route scroll events to diff view if it has content
                    if view_state.diff_view.has_content() {
                        let diff_bounds = if view_state.commit_detail_view.has_content() {
                            let (_detail_rect, diff_rect) = layout.right_panel.split_vertical(0.40);
                            diff_rect
                        } else {
                            layout.right_panel
                        };
                        if view_state.diff_view.handle_event(&input_event, diff_bounds).is_consumed() {
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

                    // Right-click context menus
                    if let InputEvent::MouseDown { button: input::MouseButton::Right, x, y, .. } = &input_event {
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
                        } else if layout.staging.contains(*x, *y)
                            && let Some(items) = view_state.staging_well.context_menu_items_at(*x, *y, layout.staging) {
                                view_state.context_menu.show(items, *x, *y);
                                return;
                            }
                    }

                    // Detect clicks on panels to switch focus
                    if let InputEvent::MouseDown { x, y, .. } = &input_event {
                        if layout.staging.contains(*x, *y) {
                            view_state.focused_panel = FocusedPanel::Staging;
                        } else if layout.graph.contains(*x, *y) {
                            view_state.focused_panel = FocusedPanel::Graph;
                        } else if layout.sidebar.contains(*x, *y) {
                            view_state.focused_panel = FocusedPanel::Sidebar;
                            view_state.branch_sidebar.set_focused(true);
                        }
                        // Update sidebar focus state based on panel focus
                        if view_state.focused_panel != FocusedPanel::Sidebar {
                            view_state.branch_sidebar.set_focused(false);
                        }
                        // Unfocus staging text inputs when focus moves away
                        if view_state.focused_panel != FocusedPanel::Staging {
                            view_state.staging_well.unfocus_all();
                        }
                    }

                    // Route to focused panel
                    match view_state.focused_panel {
                        FocusedPanel::Graph => {
                            let prev_selected = view_state.commit_graph_view.selected_commit;
                            let response = view_state.commit_graph_view.handle_event(&input_event, &repo_tab.commits, layout.graph);
                            if view_state.commit_graph_view.selected_commit != prev_selected
                                && let Some(oid) = view_state.commit_graph_view.selected_commit
                                    && view_state.last_diff_commit != Some(oid) {
                                        view_state.pending_messages.push(AppMessage::SelectedCommit(oid));
                                    }
                            if let Some(action) = view_state.commit_graph_view.take_action() {
                                match action {
                                    GraphAction::LoadMore => {
                                        view_state.pending_messages.push(AppMessage::LoadMoreCommits);
                                    }
                                }
                            }
                            if response.is_consumed() {
                                return;
                            }
                        }
                        FocusedPanel::Staging => {
                            let response = view_state.staging_well.handle_event(&input_event, layout.staging);

                            if let Some(action) = view_state.staging_well.take_action() {
                                match action {
                                    StagingAction::StageFile(path) => {
                                        view_state.pending_messages.push(AppMessage::StageFile(path));
                                    }
                                    StagingAction::UnstageFile(path) => {
                                        view_state.pending_messages.push(AppMessage::UnstageFile(path));
                                    }
                                    StagingAction::StageAll => {
                                        view_state.pending_messages.push(AppMessage::StageAll);
                                    }
                                    StagingAction::UnstageAll => {
                                        view_state.pending_messages.push(AppMessage::UnstageAll);
                                    }
                                    StagingAction::Commit(message) => {
                                        view_state.pending_messages.push(AppMessage::Commit(message));
                                    }
                                    StagingAction::AmendCommit(message) => {
                                        view_state.pending_messages.push(AppMessage::AmendCommit(message));
                                    }
                                    StagingAction::ToggleAmend => {
                                        view_state.pending_messages.push(AppMessage::ToggleAmend);
                                    }
                                    StagingAction::ViewDiff(path) => {
                                        let staged = view_state.staging_well.staged_list.files
                                            .iter().any(|f| f.path == path);
                                        view_state.pending_messages.push(AppMessage::ViewDiff(path, staged));
                                    }
                                }
                            }
                            if response.is_consumed() {
                                return;
                            }
                        }
                        FocusedPanel::Sidebar => {
                            // Keyboard events handled by branch_sidebar.handle_event above
                        }
                    }

                    // Tab to cycle panels (only when not consumed by focused panel)
                    if let InputEvent::KeyDown { key: Key::Tab, .. } = &input_event {
                        view_state.focused_panel = match view_state.focused_panel {
                            FocusedPanel::Graph => FocusedPanel::Staging,
                            FocusedPanel::Staging => FocusedPanel::Sidebar,
                            FocusedPanel::Sidebar => FocusedPanel::Graph,
                        };
                        view_state.branch_sidebar.set_focused(view_state.focused_panel == FocusedPanel::Sidebar);
                        if view_state.focused_panel != FocusedPanel::Staging {
                            view_state.staging_well.unfocus_all();
                        }
                        return;
                    }

                    // Update hover states
                    if let InputEvent::MouseMove { x, y, .. } = &input_event {
                        view_state.header_bar.update_hover(*x, *y, layout.header);
                        view_state.branch_sidebar.update_hover(*x, *y, layout.sidebar);
                        view_state.staging_well.update_hover(*x, *y, layout.staging);

                        // Determine cursor icon based on hover position
                        let cursor = determine_cursor(*x, *y, &layout, view_state);

                        // Tab bar hover needs text_renderer
                        if let Some(ref render_state) = self.state {
                            if tab_count > 1 {
                                self.tab_bar.update_hover_with_renderer(*x, *y, tab_bar_bounds, &render_state.text_renderer);
                            }
                            render_state.window.set_cursor(cursor);
                        }
                    }
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

/// Determine which cursor icon to show based on mouse position.
/// Returns resize cursors near divider edges, Text cursor over text inputs, Default otherwise.
fn determine_cursor(x: f32, y: f32, layout: &ScreenLayout, view_state: &TabViewState) -> CursorIcon {
    let hit_tolerance = 5.0;

    // Check divider hover zones (only below shortcut bar)
    if y > layout.shortcut_bar.bottom() {
        // Divider 1: sidebar | graph (vertical)
        let sidebar_edge = layout.sidebar.right();
        if (x - sidebar_edge).abs() < hit_tolerance {
            return CursorIcon::ColResize;
        }

        // Divider 2: graph | right panel (vertical)
        let graph_edge = layout.graph.right();
        if (x - graph_edge).abs() < hit_tolerance {
            return CursorIcon::ColResize;
        }

        // Divider 3: staging | right panel (horizontal, only in right column)
        let staging_edge = layout.staging.bottom();
        if (y - staging_edge).abs() < hit_tolerance
            && x >= layout.staging.x
            && x <= layout.staging.right()
        {
            return CursorIcon::RowResize;
        }
    }

    // Check staging area text inputs (subject line, body area)
    if layout.staging.contains(x, y) {
        let (subject_bounds, body_bounds, _, _, _) = view_state.staging_well.compute_regions(layout.staging);
        if subject_bounds.contains(x, y) || body_bounds.contains(x, y) {
            return CursorIcon::Text;
        }
    }

    // Check search bar when active (overlays the graph area)
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

    CursorIcon::Default
}

// ============================================================================
// Rendering
// ============================================================================

/// Handle a context menu action by dispatching to the appropriate AppMessage
fn handle_context_menu_action(
    action_id: &str,
    view_state: &mut TabViewState,
    toast_manager: &mut ToastManager,
    confirm_dialog: &mut ConfirmDialog,
    branch_name_dialog: &mut BranchNameDialog,
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
                view_state.pending_messages.push(AppMessage::DiscardFile(param.to_string()));
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
        "open_submodule" | "open_worktree" => {
            toast_manager.push("Open terminal not yet implemented".to_string(), ToastSeverity::Info);
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
        "create_branch" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                let default_name = format!("branch-{}", short);
                branch_name_dialog.show(&default_name, oid);
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
        _ => {
            eprintln!("Unknown context menu action: {}", action_id);
        }
    }

    view_state.context_menu_commit = None;
}

/// Add panel backgrounds, borders, and visual chrome to the output
fn add_panel_chrome(output: &mut WidgetOutput, layout: &ScreenLayout, screen_bounds: &Rect, focused: FocusedPanel) {
    // Panel backgrounds for depth separation
    output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
        &layout.graph,
        theme::PANEL_GRAPH.to_array(),
    ));
    output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
        &layout.staging,
        theme::PANEL_STAGING.to_array(),
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

    // Vertical border: sidebar | graph (2px for drag affordance)
    output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
        &Rect::new(layout.sidebar.right(), layout.sidebar.y, 2.0, layout.sidebar.height),
        theme::BORDER.to_array(),
    ));

    // Vertical border: graph | staging/secondary (2px for drag affordance)
    output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
        &Rect::new(layout.graph.right(), layout.graph.y, 2.0, layout.graph.height),
        theme::BORDER.to_array(),
    ));

    // Horizontal border: staging | right panel (2px for drag affordance)
    output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
        &Rect::new(layout.staging.x, layout.staging.bottom(), layout.staging.width, 2.0),
        theme::BORDER.to_array(),
    ));

    // Focused panel accent top border (2px accent-colored line at top of focused panel)
    let focused_rect = match focused {
        FocusedPanel::Graph => &layout.graph,
        FocusedPanel::Staging => &layout.staging,
        FocusedPanel::Sidebar => &layout.sidebar,
    };
    output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
        &Rect::new(focused_rect.x, focused_rect.y, focused_rect.width, 2.0),
        theme::ACCENT.to_array(),
    ));
}

/// Build the UI vertices for the active tab.
/// Takes separate borrows to avoid conflict between App fields and RenderState.
#[allow(clippy::too_many_arguments)]
fn build_ui_output(
    tabs: &mut [(RepoTab, TabViewState)],
    active_tab: usize,
    tab_bar: &TabBar,
    toast_manager: &ToastManager,
    repo_dialog: &RepoDialog,
    settings_dialog: &SettingsDialog,
    confirm_dialog: &ConfirmDialog,
    branch_name_dialog: &BranchNameDialog,
    text_renderer: &TextRenderer,
    scale_factor: f64,
    extent: [u32; 2],
    avatar_cache: &mut AvatarCache,
    avatar_renderer: &AvatarRenderer,
    sidebar_ratio: f32,
    graph_ratio: f32,
    staging_ratio: f32,
    shortcut_bar_visible: bool,
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
        Some(staging_ratio),
        shortcut_bar_visible,
    );

    // Three layers: graph content renders first, chrome on top, overlay on top of everything
    let mut graph_output = WidgetOutput::new();
    let mut chrome_output = WidgetOutput::new();
    let mut overlay_output = WidgetOutput::new();

    // Panel backgrounds and borders go in graph layer (base - renders first, behind everything)
    let focused = tabs.get(active_tab).map(|(_, vs)| vs.focused_panel).unwrap_or_default();
    add_panel_chrome(&mut graph_output, &layout, &main_bounds, focused);

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
        chrome_output.extend(view_state.header_bar.layout(text_renderer, layout.header));

        // Shortcut bar (chrome layer - on top of graph) - only when visible
        if shortcut_bar_visible {
            chrome_output.extend(view_state.shortcut_bar.layout(text_renderer, layout.shortcut_bar));
        }

        // Branch sidebar (chrome layer)
        chrome_output.extend(view_state.branch_sidebar.layout(text_renderer, layout.sidebar));

        // Staging well (chrome layer)
        chrome_output.extend(view_state.staging_well.layout(text_renderer, layout.staging));

        // Right panel (chrome layer) - render diff/detail or empty state placeholder
        if view_state.commit_detail_view.has_content() {
            let (detail_rect, diff_rect) = layout.right_panel.split_vertical(0.40);
            chrome_output.extend(view_state.commit_detail_view.layout(text_renderer, detail_rect));
            if view_state.diff_view.has_content() {
                chrome_output.extend(view_state.diff_view.layout(text_renderer, diff_rect));
            }
        } else if view_state.diff_view.has_content() {
            chrome_output.extend(view_state.diff_view.layout(text_renderer, layout.right_panel));
        } else {
            // Empty state placeholder
            let msg = "Select a commit to view details";
            let msg_w = text_renderer.measure_text(msg);
            let line_h = text_renderer.line_height();
            let cx = layout.right_panel.x + (layout.right_panel.width - msg_w) / 2.0;
            let cy = layout.right_panel.y + (layout.right_panel.height - line_h) / 2.0;
            chrome_output.text_vertices.extend(text_renderer.layout_text(
                msg, cx, cy,
                theme::TEXT_MUTED.to_array(),
            ));
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
    overlay_output.extend(toast_manager.layout(text_renderer, screen_bounds));

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
    if let Some((_, view_state)) = app.tabs.get_mut(app.active_tab) {
        view_state.header_bar.update_button_state();
        view_state.staging_well.update_button_state();
        view_state.staging_well.update_cursors(now);
        view_state.commit_graph_view.search_bar.update_cursor(now);
        view_state.shortcut_bar.set_context(match view_state.focused_panel {
            FocusedPanel::Graph => ShortcutContext::Graph,
            FocusedPanel::Staging => ShortcutContext::Staging,
            FocusedPanel::Sidebar => ShortcutContext::Sidebar,
        });
        view_state.shortcut_bar.show_new_tab_hint = single_tab;
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
    let (sidebar_ratio, graph_ratio, staging_ratio) = (app.sidebar_ratio, app.graph_ratio, app.staging_ratio);
    let (graph_output, chrome_output, overlay_output) = build_ui_output(
        &mut app.tabs, app.active_tab, &app.tab_bar,
        &app.toast_manager, &app.repo_dialog, &app.settings_dialog, &app.confirm_dialog, &app.branch_name_dialog,
        &state.text_renderer, scale_factor, extent,
        &mut state.avatar_cache, &state.avatar_renderer,
        sidebar_ratio, graph_ratio, staging_ratio,
        app.shortcut_bar_visible,
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
                clear_values: vec![Some(theme::BACKGROUND.to_array().into())],
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
        state.text_renderer.draw(builder, vertex_buffer, viewport)?;
    }
    Ok(())
}

fn capture_screenshot(app: &mut App) -> Result<image::RgbaImage> {
    let state = app.state.as_mut().unwrap();
    state.previous_frame_end.as_mut().unwrap().cleanup_finished();

    let extent = state.surface.extent();
    let scale_factor = state.scale_factor;
    let (sidebar_ratio, graph_ratio, staging_ratio) = (app.sidebar_ratio, app.graph_ratio, app.staging_ratio);
    let (graph_output, chrome_output, overlay_output) = build_ui_output(
        &mut app.tabs, app.active_tab, &app.tab_bar,
        &app.toast_manager, &app.repo_dialog, &app.settings_dialog, &app.confirm_dialog, &app.branch_name_dialog,
        &state.text_renderer, scale_factor, extent,
        &mut state.avatar_cache, &state.avatar_renderer,
        sidebar_ratio, graph_ratio, staging_ratio,
        app.shortcut_bar_visible,
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
                clear_values: vec![Some(theme::BACKGROUND.to_array().into())],
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
    let (sidebar_ratio, graph_ratio, staging_ratio) = (app.sidebar_ratio, app.graph_ratio, app.staging_ratio);
    let (graph_output, chrome_output, overlay_output) = build_ui_output(
        &mut app.tabs, app.active_tab, &app.tab_bar,
        &app.toast_manager, &app.repo_dialog, &app.settings_dialog, &app.confirm_dialog, &app.branch_name_dialog,
        &state.text_renderer, scale_factor, extent,
        &mut state.avatar_cache, &state.avatar_renderer,
        sidebar_ratio, graph_ratio, staging_ratio,
        app.shortcut_bar_visible,
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
                clear_values: vec![Some(theme::BACKGROUND.to_array().into())],
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
                    view_state.commit_detail_view.set_commit(info, diff_files.clone());
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
