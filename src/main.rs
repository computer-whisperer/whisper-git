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
    window::{Window, WindowId},
};

use git2::Oid;

use crate::git::{CommitInfo, GitRepo, RemoteOpResult};
use crate::input::{InputEvent, InputState, Key};
use crate::renderer::{capture_to_buffer, OffscreenTarget, SurfaceManager, VulkanContext};
use crate::ui::{AvatarCache, AvatarRenderer, Rect, ScreenLayout, SplineRenderer, TextRenderer, Widget, WidgetOutput};
use crate::ui::widget::theme;
use crate::ui::widgets::{ContextMenu, MenuAction, MenuItem, HeaderBar, RepoDialog, RepoDialogAction, ShortcutBar, ShortcutContext, TabBar, TabAction, ToastManager, ToastSeverity};
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
    LoadMoreCommits,
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

struct App {
    cli_args: CliArgs,
    tabs: Vec<(RepoTab, TabViewState)>,
    active_tab: usize,
    tab_bar: TabBar,
    repo_dialog: RepoDialog,
    toast_manager: ToastManager,
    state: Option<RenderState>,
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

        Ok(Self {
            cli_args,
            tabs,
            active_tab: 0,
            tab_bar,
            repo_dialog: RepoDialog::new(),
            toast_manager: ToastManager::new(),
            state: None,
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
        for (repo_tab, view_state) in &mut self.tabs {
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
                            let additions = hunks.iter().flat_map(|h| &h.lines).filter(|l| l.origin == '+').count();
                            let deletions = hunks.iter().flat_map(|h| &h.lines).filter(|l| l.origin == '-').count();
                            let diff_file = crate::git::DiffFile {
                                path: path.clone(),
                                hunks,
                                additions,
                                deletions,
                            };
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
                            self.toast_manager.push(
                                format!("Delete failed: {}", e),
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
                                    let additions = hunks.iter().flat_map(|h| &h.lines).filter(|l| l.origin == '+').count();
                                    let deletions = hunks.iter().flat_map(|h| &h.lines).filter(|l| l.origin == '-').count();
                                    let diff_file = crate::git::DiffFile {
                                        path: path.clone(),
                                        hunks,
                                        additions,
                                        deletions,
                                    };
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
                                    let additions = hunks.iter().flat_map(|h| &h.lines).filter(|l| l.origin == '+').count();
                                    let deletions = hunks.iter().flat_map(|h| &h.lines).filter(|l| l.origin == '-').count();
                                    let diff_file = crate::git::DiffFile {
                                        path: path.clone(),
                                        hunks,
                                        additions,
                                        deletions,
                                    };
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
                AppMessage::LoadMoreCommits => {
                    let current_count = repo_tab.commits.len();
                    let new_count = current_count + 50;
                    if let Ok(commits) = repo!().commit_graph(new_count) {
                        repo_tab.commits = commits;
                        view_state.commit_graph_view.update_layout(&repo_tab.commits);
                    }
                    view_state.commit_graph_view.finish_loading();
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
    view_state.staging_well.scale = scale;

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
            view_state.secondary_repos_view.set_submodules(submodules);
        }
        if let Ok(worktrees) = repo.worktrees() {
            view_state.secondary_repos_view.set_worktrees(worktrees);
        }
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
                // Sync metrics on all tabs
                for (_, view_state) in &mut self.tabs {
                    view_state.commit_graph_view.sync_metrics(&state.text_renderer);
                    view_state.branch_sidebar.sync_metrics(&state.text_renderer);
                    view_state.staging_well.scale = scale_factor as f32;
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
                    let layout = ScreenLayout::compute_with_gap(main_bounds, 4.0, scale);

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
                                        );
                                    }
                                }
                            }
                            return;
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
                    let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else { return };

                    // Handle per-tab global keys (except Tab, which is handled after panel routing)
                    if let InputEvent::KeyDown { key, .. } = &input_event
                        && key == &Key::Escape {
                            if view_state.diff_view.has_content() {
                                view_state.diff_view.clear();
                                view_state.last_diff_commit = None;
                            } else if view_state.commit_detail_view.has_content() {
                                view_state.commit_detail_view.clear();
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
                                    view_state.pending_messages.push(AppMessage::DeleteBranch(name));
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
                                    println!("Help: Tab to switch panels, j/k to navigate, Space to stage/unstage");
                                }
                                HeaderAction::Settings => {
                                    println!("Settings not yet implemented");
                                }
                            }
                        }
                        return;
                    }

                    // Route to commit detail view when active
                    if view_state.commit_detail_view.has_content() {
                        let (detail_rect, _diff_rect) = layout.secondary_repos.split_vertical(0.40);
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
                            let (_detail_rect, diff_rect) = layout.secondary_repos.split_vertical(0.40);
                            diff_rect
                        } else {
                            layout.secondary_repos
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
                        // Tab bar hover needs text_renderer
                        if self.tabs.len() > 1
                            && let Some(ref render_state) = self.state {
                                self.tab_bar.update_hover_with_renderer(*x, *y, tab_bar_bounds, &render_state.text_renderer);
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

// ============================================================================
// Rendering
// ============================================================================

/// Handle a context menu action by dispatching to the appropriate AppMessage
fn handle_context_menu_action(
    action_id: &str,
    view_state: &mut TabViewState,
    toast_manager: &mut ToastManager,
) {
    // Actions may be in format "action:param" or just "action"
    let (action, param) = action_id.split_once(':').unwrap_or((action_id, ""));

    match action {
        // Commit graph actions
        "copy_sha" => {
            if let Some(oid) = view_state.context_menu_commit {
                // Use arboard for clipboard if available, otherwise just print
                let sha = oid.to_string();
                // Clipboard integration can be added later (e.g., arboard crate)
                toast_manager.push(
                    format!("SHA: {}", &sha[..7]),
                    ToastSeverity::Info,
                );
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
                view_state.pending_messages.push(AppMessage::DeleteBranch(param.to_string()));
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
                // Discard changes: checkout the file from HEAD
                toast_manager.push(
                    format!("Discard not yet implemented for {}", param),
                    ToastSeverity::Info,
                );
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
        &layout.secondary_repos,
        theme::PANEL_STAGING.to_array(),
    ));

    // Border below shortcut bar (full width of screen)
    output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
        &Rect::new(0.0, layout.shortcut_bar.bottom(), screen_bounds.width, 1.0),
        theme::BORDER.to_array(),
    ));

    // Vertical border: sidebar | graph
    output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
        &Rect::new(layout.sidebar.right(), layout.sidebar.y, 1.0, layout.sidebar.height),
        theme::BORDER.to_array(),
    ));

    // Vertical border: graph | staging/secondary
    output.spline_vertices.extend(crate::ui::widget::create_rect_vertices(
        &Rect::new(layout.graph.right(), layout.graph.y, 1.0, layout.graph.height),
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
    text_renderer: &TextRenderer,
    scale_factor: f64,
    extent: [u32; 2],
    avatar_cache: &mut AvatarCache,
    avatar_renderer: &AvatarRenderer,
) -> WidgetOutput {
    let screen_bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
    let scale = scale_factor as f32;

    // Tab bar takes space at top when multiple tabs
    let tab_bar_height = if tabs.len() > 1 { TabBar::height(scale) } else { 0.0 };
    let (tab_bar_bounds, main_bounds) = screen_bounds.take_top(tab_bar_height);
    let layout = ScreenLayout::compute_with_gap(main_bounds, 4.0, scale);

    let mut output = WidgetOutput::new();

    // Panel backgrounds and borders
    let focused = tabs.get(active_tab).map(|(_, vs)| vs.focused_panel).unwrap_or_default();
    add_panel_chrome(&mut output, &layout, &main_bounds, focused);

    // Tab bar (only when multiple tabs)
    if tabs.len() > 1 {
        output.extend(tab_bar.layout(text_renderer, tab_bar_bounds));
    }

    // Active tab views
    if let Some((repo_tab, view_state)) = tabs.get_mut(active_tab) {
        // Header bar
        output.extend(view_state.header_bar.layout(text_renderer, layout.header));

        // Shortcut bar (below header)
        output.extend(view_state.shortcut_bar.layout(text_renderer, layout.shortcut_bar));

        // Branch sidebar
        output.extend(view_state.branch_sidebar.layout(text_renderer, layout.sidebar));

        // Commit graph
        let spline_vertices = view_state.commit_graph_view.layout_splines(text_renderer, &repo_tab.commits, layout.graph);
        let (text_vertices, pill_vertices, av_vertices) = view_state.commit_graph_view.layout_text(
            text_renderer, &repo_tab.commits, layout.graph,
            avatar_cache, avatar_renderer,
        );
        output.spline_vertices.extend(spline_vertices);
        output.spline_vertices.extend(pill_vertices);
        output.text_vertices.extend(text_vertices);
        output.avatar_vertices.extend(av_vertices);

        // Staging well
        output.extend(view_state.staging_well.layout(text_renderer, layout.staging));

        // Right panel
        if view_state.commit_detail_view.has_content() {
            let (detail_rect, diff_rect) = layout.secondary_repos.split_vertical(0.40);
            output.extend(view_state.commit_detail_view.layout(text_renderer, detail_rect));
            if view_state.diff_view.has_content() {
                output.extend(view_state.diff_view.layout(text_renderer, diff_rect));
            }
        } else if view_state.diff_view.has_content() {
            output.extend(view_state.diff_view.layout(text_renderer, layout.secondary_repos));
        } else {
            output.extend(view_state.secondary_repos_view.layout(text_renderer, layout.secondary_repos));
        }
    }

    // Context menu overlay (on top of panels)
    if let Some((_, view_state)) = tabs.get_mut(active_tab)
        && view_state.context_menu.is_visible() {
            output.extend(view_state.context_menu.layout(text_renderer, screen_bounds));
        }

    // Toast notifications (rendered on top of context menus)
    output.extend(toast_manager.layout(text_renderer, screen_bounds));

    // Repo dialog (on top of everything including toasts)
    if repo_dialog.is_visible() {
        output.extend(repo_dialog.layout(text_renderer, screen_bounds));
    }

    output
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
    if let Some((_, view_state)) = app.tabs.get_mut(app.active_tab) {
        view_state.header_bar.update_button_state();
        view_state.staging_well.update_button_state();
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
    let output = build_ui_output(
        &mut app.tabs, app.active_tab, &app.tab_bar,
        &app.toast_manager, &app.repo_dialog,
        &state.text_renderer, scale_factor, extent,
        &mut state.avatar_cache, &state.avatar_renderer,
    );

    let viewport = Viewport {
        offset: [0.0, 0.0],
        extent: [extent[0] as f32, extent[1] as f32],
        depth_range: 0.0..=1.0,
    };

    // Build command buffer
    let mut builder = AutoCommandBufferBuilder::primary(
        state.ctx.command_buffer_allocator.clone(),
        state.ctx.queue.queue_family_index(),
        CommandBufferUsage::OneTimeSubmit,
    )
    .context("Failed to create command buffer")?;

    // Upload avatar atlas if dirty (before render pass)
    state.avatar_renderer.upload_atlas(&mut builder)?;

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

    render_output_to_builder(&mut builder, state, output, viewport)?;

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
    let output = build_ui_output(
        &mut app.tabs, app.active_tab, &app.tab_bar,
        &app.toast_manager, &app.repo_dialog,
        &state.text_renderer, scale_factor, extent,
        &mut state.avatar_cache, &state.avatar_renderer,
    );

    let state = app.state.as_mut().unwrap();
    let viewport = Viewport {
        offset: [0.0, 0.0],
        extent: [extent[0] as f32, extent[1] as f32],
        depth_range: 0.0..=1.0,
    };

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

    render_output_to_builder(&mut builder, state, output, viewport)?;

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
    let output = build_ui_output(
        &mut app.tabs, app.active_tab, &app.tab_bar,
        &app.toast_manager, &app.repo_dialog,
        &state.text_renderer, scale_factor, extent,
        &mut state.avatar_cache, &state.avatar_renderer,
    );

    let state = app.state.as_mut().unwrap();
    let viewport = Viewport {
        offset: [0.0, 0.0],
        extent: [extent[0] as f32, extent[1] as f32],
        depth_range: 0.0..=1.0,
    };

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

    render_output_to_builder(&mut builder, state, output, viewport)?;

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
                    MenuItem { label: "Copy SHA".to_string(), shortcut: None, action_id: "copy_sha".to_string() },
                    MenuItem { label: "View Details".to_string(), shortcut: Some("Enter".to_string()), action_id: "view_details".to_string() },
                    MenuItem { label: "Checkout".to_string(), shortcut: None, action_id: "checkout".to_string() },
                ];
                view_state.context_menu.show(items, cx, cy);
            }
        }
        "commit-detail" => {
            if let Some((repo_tab, view_state)) = app.tabs.get_mut(app.active_tab) {
                if let Some(first) = repo_tab.commits.first() {
                    let oid = first.id;
                    if let Some(ref repo) = repo_tab.repo {
                        if let Ok(info) = repo.full_commit_info(oid) {
                            let diff_files = repo.diff_for_commit(oid).unwrap_or_default();
                            view_state.commit_detail_view.set_commit(info, diff_files.clone());
                            if let Some(first_file) = diff_files.first() {
                                let title = first_file.path.clone();
                                view_state.diff_view.set_diff(vec![first_file.clone()], title);
                            }
                        }
                    }
                }
            }
        }
        other => {
            eprintln!("Unknown screenshot state: '{}'. Valid states: open-dialog, search, context-menu, commit-detail", other);
        }
    }
}
