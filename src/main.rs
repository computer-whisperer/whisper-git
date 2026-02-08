// Allow dead code for APIs intended for future phases
#![allow(dead_code)]

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
    command_buffer::{AutoCommandBufferBuilder, CommandBufferUsage, RenderPassBeginInfo},
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
use crate::ui::{Rect, ScreenLayout, SplineRenderer, TextRenderer, Widget, WidgetOutput};
use crate::ui::widgets::HeaderBar;
use crate::views::{BranchSidebar, CommitGraphView, DiffView, SecondaryReposView, StagingWell, StagingAction, SidebarAction};

// ============================================================================
// CLI
// ============================================================================

#[derive(Default)]
struct CliArgs {
    screenshot: Option<PathBuf>,
    screenshot_size: Option<(u32, u32)>,
    screenshot_scale: Option<f64>,
    view: Option<String>,
    repo: Option<PathBuf>,
}

fn parse_args() -> CliArgs {
    let mut args = CliArgs::default();
    let mut iter = std::env::args().skip(1);

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--screenshot" => args.screenshot = iter.next().map(PathBuf::from),
            "--size" => {
                // Parse WxH format (e.g., "1920x1080")
                if let Some(size_str) = iter.next() {
                    if let Some((w, h)) = size_str.split_once('x') {
                        if let (Ok(width), Ok(height)) = (w.parse(), h.parse()) {
                            args.screenshot_size = Some((width, height));
                        }
                    }
                }
            }
            "--scale" => {
                if let Some(s) = iter.next() {
                    args.screenshot_scale = s.parse().ok();
                }
            }
            "--view" => args.view = iter.next(),
            "--repo" => args.repo = iter.next().map(PathBuf::from),
            other if !other.starts_with('-') => args.repo = Some(PathBuf::from(other)),
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
    Push,
    SelectedCommit(Oid),
    ViewDiff(String, bool), // (path, staged)
    CheckoutBranch(String),
    CheckoutRemoteBranch(String, String),
    DeleteBranch(String),
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
    repo: Option<GitRepo>,
    commits: Vec<CommitInfo>,
    state: Option<RenderState>,
}

/// Initialized state (after window creation)
struct RenderState {
    window: Arc<Window>,
    ctx: VulkanContext,
    surface: SurfaceManager,
    text_renderer: TextRenderer,
    spline_renderer: SplineRenderer,
    previous_frame_end: Option<Box<dyn GpuFuture>>,
    frame_count: u32,
    scale_factor: f64,
    // UI components
    input_state: InputState,
    focused_panel: FocusedPanel,
    header_bar: HeaderBar,
    branch_sidebar: BranchSidebar,
    commit_graph_view: CommitGraphView,
    staging_well: StagingWell,
    secondary_repos_view: SecondaryReposView,
    diff_view: DiffView,
    /// Track which commit we last loaded a diff for
    last_diff_commit: Option<Oid>,
    // Pending messages
    pending_messages: Vec<AppMessage>,
    /// Receiver for background fetch operation
    fetch_receiver: Option<Receiver<RemoteOpResult>>,
    /// Receiver for background push operation
    push_receiver: Option<Receiver<RemoteOpResult>>,
    /// Status message to display (message, time set)
    status_message: Option<(String, Instant)>,
}

impl App {
    fn new(cli_args: CliArgs) -> Result<Self> {
        // Load commits from repo
        let repo_path = cli_args.repo.as_deref().unwrap_or(".".as_ref());
        let (repo, commits) = match GitRepo::open(repo_path) {
            Ok(repo) => {
                let commits = repo.commit_graph(50).unwrap_or_default();
                let location: String = repo.workdir()
                    .map(|p| format!("{:?}", p))
                    .unwrap_or_else(|| format!("{:?} (bare)", repo.repo_name()));
                println!("Loaded {} commits from {}", commits.len(), location);
                (Some(repo), commits)
            }
            Err(e) => {
                eprintln!("Warning: Could not open repository: {e}");
                (None, Vec::new())
            }
        };

        Ok(Self {
            cli_args,
            repo,
            commits,
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

        // Initialize UI components
        let mut header_bar = HeaderBar::new();
        let mut branch_sidebar = BranchSidebar::new();
        let mut commit_graph_view = CommitGraphView::new();
        let staging_well = StagingWell::new();
        let mut secondary_repos_view = SecondaryReposView::new();

        // Sync view metrics to the current text renderer scale
        commit_graph_view.sync_metrics(&text_renderer);
        branch_sidebar.sync_metrics(&text_renderer);

        // Set up graph view with repo data
        commit_graph_view.update_layout(&self.commits);

        if let Some(ref repo) = self.repo {
            // Set repo info in header
            let repo_name = repo.repo_name();
            let branch = repo.current_branch().unwrap_or_else(|_| "unknown".to_string());
            let (ahead, behind) = repo.ahead_behind().unwrap_or((0, 0));
            header_bar.set_repo_info(repo_name, branch, ahead, behind);

            // Cache branch/tag data to avoid duplicate calls
            let branch_tips = repo.branch_tips().unwrap_or_default();
            let tags = repo.tags().unwrap_or_default();
            let current = repo.current_branch().unwrap_or_default();

            // Set HEAD and branch info in graph
            commit_graph_view.head_oid = repo.head_oid().ok();
            commit_graph_view.branch_tips = branch_tips.clone();
            commit_graph_view.tags = tags.clone();
            commit_graph_view.working_dir_status = repo.status().ok();

            // Set staging status
            header_bar.has_staged = repo.status()
                .map(|s| !s.staged.is_empty())
                .unwrap_or(false);

            // Load submodules and worktrees
            if let Ok(submodules) = repo.submodules() {
                secondary_repos_view.set_submodules(submodules);
            }
            if let Ok(worktrees) = repo.worktrees() {
                secondary_repos_view.set_worktrees(worktrees);
            }

            // Populate branch sidebar
            branch_sidebar.set_branch_data(&branch_tips, &tags, current);
        }

        self.state = Some(RenderState {
            window,
            ctx,
            surface: surface_mgr,
            text_renderer,
            spline_renderer,
            commit_graph_view,
            previous_frame_end,
            frame_count: 0,
            scale_factor: window_scale,
            input_state: InputState::new(),
            focused_panel: FocusedPanel::Graph,
            header_bar,
            branch_sidebar,
            staging_well,
            secondary_repos_view,
            diff_view: DiffView::new(),
            last_diff_commit: None,
            pending_messages: Vec::new(),
            fetch_receiver: None,
            push_receiver: None,
            status_message: None,
        });

        // Initial status refresh
        self.refresh_status();

        Ok(())
    }

    fn refresh_status(&mut self) {
        let Some(state) = &mut self.state else { return };
        let Some(ref repo) = self.repo else { return };

        // Update working directory status
        if let Ok(status) = repo.status() {
            state.commit_graph_view.working_dir_status = Some(status.clone());
            state.staging_well.update_status(&status);
            state.header_bar.has_staged = !status.staged.is_empty();
        }

        // Update ahead/behind
        if let Ok((ahead, behind)) = repo.ahead_behind() {
            state.header_bar.ahead = ahead;
            state.header_bar.behind = behind;
        }
    }

    fn process_messages(&mut self) {
        // Extract messages to avoid borrow conflicts
        let messages: Vec<_> = if let Some(state) = &mut self.state {
            state.pending_messages.drain(..).collect()
        } else {
            return;
        };

        if messages.is_empty() {
            return;
        }

        let Some(ref repo) = self.repo else { return };

        for msg in messages {
            match msg {
                AppMessage::StageFile(path) => {
                    if let Err(e) = repo.stage_file(&path) {
                        eprintln!("Failed to stage {}: {}", path, e);
                    }
                }
                AppMessage::UnstageFile(path) => {
                    if let Err(e) = repo.unstage_file(&path) {
                        eprintln!("Failed to unstage {}: {}", path, e);
                    }
                }
                AppMessage::StageAll => {
                    if let Ok(status) = repo.status() {
                        for file in &status.unstaged {
                            let _ = repo.stage_file(&file.path);
                        }
                    }
                }
                AppMessage::UnstageAll => {
                    if let Ok(status) = repo.status() {
                        for file in &status.staged {
                            let _ = repo.unstage_file(&file.path);
                        }
                    }
                }
                AppMessage::Commit(message) => {
                    match repo.commit(&message) {
                        Ok(oid) => {
                            println!("Created commit: {}", oid);
                            // Refresh commits
                            self.commits = repo.commit_graph(50).unwrap_or_default();
                            if let Some(state) = &mut self.state {
                                state.commit_graph_view.update_layout(&self.commits);
                                state.commit_graph_view.head_oid = repo.head_oid().ok();
                                state.staging_well.clear_message();
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to commit: {}", e);
                        }
                    }
                }
                AppMessage::Fetch => {
                    if let Some(state) = &self.state {
                        if state.fetch_receiver.is_some() {
                            eprintln!("Fetch already in progress");
                            continue;
                        }
                    }
                    if let Some(workdir) = repo.working_dir_path() {
                        let remote = repo.default_remote().unwrap_or_else(|_| "origin".to_string());
                        println!("Fetching from {}...", remote);
                        let rx = crate::git::fetch_remote_async(workdir, remote);
                        if let Some(state) = &mut self.state {
                            state.fetch_receiver = Some(rx);
                            state.header_bar.fetching = true;
                        }
                    } else {
                        eprintln!("No working directory for fetch");
                    }
                }
                AppMessage::Push => {
                    if let Some(state) = &self.state {
                        if state.push_receiver.is_some() {
                            eprintln!("Push already in progress");
                            continue;
                        }
                    }
                    if let Some(workdir) = repo.working_dir_path() {
                        let remote = repo.default_remote().unwrap_or_else(|_| "origin".to_string());
                        let branch = repo.current_branch().unwrap_or_else(|_| "HEAD".to_string());
                        println!("Pushing {} to {}...", branch, remote);
                        let rx = crate::git::push_remote_async(workdir, remote, branch);
                        if let Some(state) = &mut self.state {
                            state.push_receiver = Some(rx);
                            state.header_bar.pushing = true;
                        }
                    } else {
                        eprintln!("No working directory for push");
                    }
                }
                AppMessage::SelectedCommit(oid) => {
                    match repo.diff_for_commit(oid) {
                        Ok(diff_files) => {
                            let title = self.commits.iter()
                                .find(|c| c.id == oid)
                                .map(|c| format!("{} {}", c.short_id, c.summary))
                                .unwrap_or_else(|| oid.to_string());
                            if let Some(state) = &mut self.state {
                                state.diff_view.set_diff(diff_files, title);
                                state.last_diff_commit = Some(oid);
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to load diff for {}: {}", oid, e);
                        }
                    }
                }
                AppMessage::ViewDiff(path, staged) => {
                    match repo.diff_working_file(&path, staged) {
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
                            if let Some(state) = &mut self.state {
                                state.diff_view.set_diff(vec![diff_file], title);
                                state.last_diff_commit = None;
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to load diff for {}: {}", path, e);
                        }
                    }
                }
                AppMessage::CheckoutBranch(name) => {
                    match repo.checkout_branch(&name) {
                        Ok(()) => {
                            println!("Checked out branch: {}", name);
                            // Full refresh
                            self.commits = repo.commit_graph(50).unwrap_or_default();
                            if let Some(state) = &mut self.state {
                                state.commit_graph_view.update_layout(&self.commits);
                                state.commit_graph_view.head_oid = repo.head_oid().ok();
                                let branch_tips = repo.branch_tips().unwrap_or_default();
                                let tags = repo.tags().unwrap_or_default();
                                let current = repo.current_branch().unwrap_or_default();
                                state.commit_graph_view.branch_tips = branch_tips.clone();
                                state.commit_graph_view.tags = tags.clone();
                                state.branch_sidebar.set_branch_data(&branch_tips, &tags, current.clone());
                                state.header_bar.set_repo_info(
                                    state.header_bar.repo_name.clone(),
                                    current,
                                    repo.ahead_behind().map(|(a,_)| a).unwrap_or(0),
                                    repo.ahead_behind().map(|(_,b)| b).unwrap_or(0),
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to checkout branch '{}': {}", name, e);
                        }
                    }
                }
                AppMessage::CheckoutRemoteBranch(remote, branch) => {
                    match repo.checkout_remote_branch(&remote, &branch) {
                        Ok(()) => {
                            println!("Checked out remote branch: {}/{}", remote, branch);
                            // Full refresh (same as CheckoutBranch)
                            self.commits = repo.commit_graph(50).unwrap_or_default();
                            if let Some(state) = &mut self.state {
                                state.commit_graph_view.update_layout(&self.commits);
                                state.commit_graph_view.head_oid = repo.head_oid().ok();
                                let branch_tips = repo.branch_tips().unwrap_or_default();
                                let tags = repo.tags().unwrap_or_default();
                                let current = repo.current_branch().unwrap_or_default();
                                state.commit_graph_view.branch_tips = branch_tips.clone();
                                state.commit_graph_view.tags = tags.clone();
                                state.branch_sidebar.set_branch_data(&branch_tips, &tags, current.clone());
                                state.header_bar.set_repo_info(
                                    state.header_bar.repo_name.clone(),
                                    current,
                                    repo.ahead_behind().map(|(a,_)| a).unwrap_or(0),
                                    repo.ahead_behind().map(|(_,b)| b).unwrap_or(0),
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to checkout remote branch '{}/{}': {}", remote, branch, e);
                        }
                    }
                }
                AppMessage::DeleteBranch(name) => {
                    match repo.delete_branch(&name) {
                        Ok(()) => {
                            println!("Deleted branch: {}", name);
                            // Refresh branch data
                            if let Some(state) = &mut self.state {
                                let branch_tips = repo.branch_tips().unwrap_or_default();
                                let tags = repo.tags().unwrap_or_default();
                                let current = repo.current_branch().unwrap_or_default();
                                state.branch_sidebar.set_branch_data(&branch_tips, &tags, current);
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to delete branch '{}': {}", name, e);
                        }
                    }
                }
            }
        }

        // Refresh status after processing all messages
        self.refresh_status();
    }

    fn poll_remote_ops(&mut self) {
        let Some(state) = &mut self.state else { return };

        // Poll fetch
        if let Some(ref rx) = state.fetch_receiver {
            if let Ok(result) = rx.try_recv() {
                state.header_bar.fetching = false;
                state.fetch_receiver = None;
                if result.success {
                    println!("Fetch completed successfully");
                    if !result.error.is_empty() {
                        // git fetch writes progress to stderr even on success
                        println!("{}", result.error.trim());
                    }
                    state.status_message = Some(("Fetch complete".to_string(), Instant::now()));
                    // Refresh repo data
                    if let Some(ref repo) = self.repo {
                        self.commits = repo.commit_graph(50).unwrap_or_default();
                        state.commit_graph_view.update_layout(&self.commits);
                        state.commit_graph_view.head_oid = repo.head_oid().ok();
                        state.commit_graph_view.branch_tips = repo.branch_tips().unwrap_or_default();
                        state.commit_graph_view.tags = repo.tags().unwrap_or_default();
                        if let Ok((ahead, behind)) = repo.ahead_behind() {
                            state.header_bar.ahead = ahead;
                            state.header_bar.behind = behind;
                        }
                        // Refresh sidebar
                        let branch_tips = repo.branch_tips().unwrap_or_default();
                        let tags = repo.tags().unwrap_or_default();
                        let current = repo.current_branch().unwrap_or_default();
                        state.branch_sidebar.set_branch_data(&branch_tips, &tags, current);
                    }
                } else {
                    eprintln!("Fetch failed: {}", result.error);
                    state.status_message = Some((format!("Fetch failed: {}", result.error.lines().next().unwrap_or("unknown error")), Instant::now()));
                }
            }
        }

        // Poll push
        if let Some(ref rx) = state.push_receiver {
            if let Ok(result) = rx.try_recv() {
                state.header_bar.pushing = false;
                state.push_receiver = None;
                if result.success {
                    println!("Push completed successfully");
                    if !result.error.is_empty() {
                        println!("{}", result.error.trim());
                    }
                    state.status_message = Some(("Push complete".to_string(), Instant::now()));
                    // Refresh ahead/behind
                    if let Some(ref repo) = self.repo {
                        if let Ok((ahead, behind)) = repo.ahead_behind() {
                            state.header_bar.ahead = ahead;
                            state.header_bar.behind = behind;
                        }
                    }
                } else {
                    eprintln!("Push failed: {}", result.error);
                    state.status_message = Some((format!("Push failed: {}", result.error.lines().next().unwrap_or("unknown error")), Instant::now()));
                }
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_none() {
            if let Err(e) = self.init_state(event_loop) {
                eprintln!("Failed to initialize: {e:?}");
                event_loop.exit();
            }
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
                state.commit_graph_view.sync_metrics(&state.text_renderer);
                state.branch_sidebar.sync_metrics(&state.text_renderer);
                state.surface.needs_recreate = true;
            }

            WindowEvent::RedrawRequested => {
                // Poll background remote operations
                self.poll_remote_ops();
                // Process any pending messages
                self.process_messages();

                if let Err(e) = draw_frame(&mut self.state, &self.commits) {
                    eprintln!("Draw error: {e:?}");
                }

                // Screenshot mode
                let Some(state) = &mut self.state else { return };
                if let Some(ref path) = self.cli_args.screenshot {
                    if state.frame_count == 3 {
                        let result = if let Some((width, height)) = self.cli_args.screenshot_size {
                            capture_screenshot_offscreen(state, &self.commits, width, height)
                        } else {
                            capture_screenshot(state, &self.commits)
                        };
                        match result {
                            Ok(img) => {
                                if let Err(e) = img.save(path) {
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

                state.window.request_redraw();
            }

            // Handle input events
            ref win_event => {
                // Convert winit event to our InputEvent
                if let Some(input_event) = state.input_state.handle_window_event(win_event) {
                    // Calculate layout
                    let extent = state.surface.extent();
                    let screen_bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
                    let layout = ScreenLayout::compute_with_gap(screen_bounds, 4.0, state.scale_factor as f32);

                    // Handle global keys first
                    if let InputEvent::KeyDown { key, .. } = &input_event {
                        match key {
                            Key::Escape => {
                                if state.diff_view.has_content() {
                                    state.diff_view.clear();
                                    state.last_diff_commit = None;
                                } else {
                                    event_loop.exit();
                                }
                                return;
                            }
                            Key::Tab => {
                                // Cycle focus between panels
                                state.focused_panel = match state.focused_panel {
                                    FocusedPanel::Graph => FocusedPanel::Staging,
                                    FocusedPanel::Staging => FocusedPanel::Sidebar,
                                    FocusedPanel::Sidebar => FocusedPanel::Graph,
                                };
                                // Update focus states
                                state.branch_sidebar.set_focused(state.focused_panel == FocusedPanel::Sidebar);
                            }
                            _ => {}
                        }
                    }

                    // Route to branch sidebar
                    if state.branch_sidebar.handle_event(&input_event, layout.sidebar).is_consumed() {
                        // Check for sidebar actions
                        if let Some(action) = state.branch_sidebar.take_action() {
                            match action {
                                SidebarAction::CheckoutBranch(name) => {
                                    state.pending_messages.push(AppMessage::CheckoutBranch(name));
                                }
                                SidebarAction::CheckoutRemoteBranch(remote, branch) => {
                                    state.pending_messages.push(AppMessage::CheckoutRemoteBranch(remote, branch));
                                }
                                SidebarAction::DeleteBranch(name) => {
                                    state.pending_messages.push(AppMessage::DeleteBranch(name));
                                }
                            }
                        }
                        return;
                    }

                    // Route to header bar
                    if state.header_bar.handle_event(&input_event, layout.header).is_consumed() {
                        // Check for header actions
                        if let Some(action) = state.header_bar.take_action() {
                            use crate::ui::widgets::HeaderAction;
                            match action {
                                HeaderAction::Fetch => {
                                    state.pending_messages.push(AppMessage::Fetch);
                                }
                                HeaderAction::Push => {
                                    state.pending_messages.push(AppMessage::Push);
                                }
                                HeaderAction::Commit => {
                                    // Focus staging well for commit
                                    state.focused_panel = FocusedPanel::Staging;
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

                    // Route scroll events to diff view if it has content and cursor is in its area
                    if state.diff_view.has_content() {
                        if state.diff_view.handle_event(&input_event, layout.secondary_repos).is_consumed() {
                            return;
                        }
                    }

                    // Route to focused panel
                    match state.focused_panel {
                        FocusedPanel::Graph => {
                            let prev_selected = state.commit_graph_view.selected_commit;
                            let response = state.commit_graph_view.handle_event(&input_event, &self.commits, layout.graph);
                            // Check selection change regardless of consumed state
                            if state.commit_graph_view.selected_commit != prev_selected {
                                if let Some(oid) = state.commit_graph_view.selected_commit {
                                    if state.last_diff_commit != Some(oid) {
                                        state.pending_messages.push(AppMessage::SelectedCommit(oid));
                                    }
                                }
                            }
                            // But only skip further processing if consumed
                            if response.is_consumed() {
                                return;
                            }
                        }
                        FocusedPanel::Staging => {
                            state.staging_well.handle_event(&input_event, layout.staging);

                            // Check for staging actions
                            if let Some(action) = state.staging_well.take_action() {
                                match action {
                                    StagingAction::StageFile(path) => {
                                        state.pending_messages.push(AppMessage::StageFile(path));
                                    }
                                    StagingAction::UnstageFile(path) => {
                                        state.pending_messages.push(AppMessage::UnstageFile(path));
                                    }
                                    StagingAction::StageAll => {
                                        state.pending_messages.push(AppMessage::StageAll);
                                    }
                                    StagingAction::UnstageAll => {
                                        state.pending_messages.push(AppMessage::UnstageAll);
                                    }
                                    StagingAction::Commit(message) => {
                                        state.pending_messages.push(AppMessage::Commit(message));
                                    }
                                    StagingAction::ViewDiff(path) => {
                                        // Determine if the file is staged or unstaged
                                        let staged = state.staging_well.staged_list.files
                                            .iter().any(|f| f.path == path);
                                        state.pending_messages.push(AppMessage::ViewDiff(path, staged));
                                    }
                                }
                            }
                        }
                        FocusedPanel::Sidebar => {
                            // Keyboard events handled by branch_sidebar.handle_event above
                        }
                    }

                    // Update hover states
                    if let InputEvent::MouseMove { x, y, .. } = &input_event {
                        state.header_bar.update_hover(*x, *y, layout.header);
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

fn draw_frame(state_opt: &mut Option<RenderState>, commits: &[CommitInfo]) -> Result<()> {
    let state = state_opt.as_mut().unwrap();
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

    // Build UI
    let extent = state.surface.extent();
    let screen_bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
    let layout = ScreenLayout::compute_with_gap(screen_bounds, 4.0, state.scale_factor as f32);

    // Collect all vertices
    let mut output = WidgetOutput::new();

    // Header bar
    output.extend(state.header_bar.layout(&state.text_renderer, layout.header));

    // Branch sidebar
    output.extend(state.branch_sidebar.layout(&state.text_renderer, layout.sidebar));

    // Commit graph (in graph area)
    let spline_vertices = state.commit_graph_view.layout_splines(&state.text_renderer, commits, layout.graph);
    let (text_vertices, pill_vertices) = state.commit_graph_view.layout_text(&state.text_renderer, commits, layout.graph);
    output.spline_vertices.extend(spline_vertices);
    output.spline_vertices.extend(pill_vertices);
    output.text_vertices.extend(text_vertices);

    // Staging well
    output.extend(state.staging_well.layout(&state.text_renderer, layout.staging));

    // Diff view replaces secondary repos area when active
    if state.diff_view.has_content() {
        output.extend(state.diff_view.layout(&state.text_renderer, layout.secondary_repos));
    } else {
        output.extend(state.secondary_repos_view.layout(&state.text_renderer, layout.secondary_repos));
    }

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

    // Classic dark mode background
    let bg_color = [0.051f32, 0.051, 0.051, 1.0]; // #0d0d0d

    builder
        .begin_render_pass(
            RenderPassBeginInfo {
                clear_values: vec![Some(bg_color.into())],
                ..RenderPassBeginInfo::framebuffer(
                    state.surface.framebuffers[image_index as usize].clone(),
                )
            },
            Default::default(),
        )
        .context("Failed to begin render pass")?;

    // Draw splines first (background)
    if !output.spline_vertices.is_empty() {
        let spline_buffer = state.spline_renderer.create_vertex_buffer(output.spline_vertices)?;
        state.spline_renderer.draw(&mut builder, spline_buffer, viewport.clone())?;
    }

    // Draw text on top
    if !output.text_vertices.is_empty() {
        let vertex_buffer = state.text_renderer.create_vertex_buffer(output.text_vertices)?;
        state.text_renderer.draw(&mut builder, vertex_buffer, viewport)?;
    }

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

fn capture_screenshot(state: &mut RenderState, commits: &[CommitInfo]) -> Result<image::RgbaImage> {
    state.previous_frame_end.as_mut().unwrap().cleanup_finished();

    // Build UI
    let extent = state.surface.extent();
    let screen_bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
    let layout = ScreenLayout::compute_with_gap(screen_bounds, 4.0, state.scale_factor as f32);

    // Collect all vertices
    let mut output = WidgetOutput::new();

    // Header bar
    output.extend(state.header_bar.layout(&state.text_renderer, layout.header));

    // Branch sidebar
    output.extend(state.branch_sidebar.layout(&state.text_renderer, layout.sidebar));

    // Commit graph
    let spline_vertices = state.commit_graph_view.layout_splines(&state.text_renderer, commits, layout.graph);
    let (text_vertices, pill_vertices) = state.commit_graph_view.layout_text(&state.text_renderer, commits, layout.graph);
    output.spline_vertices.extend(spline_vertices);
    output.spline_vertices.extend(pill_vertices);
    output.text_vertices.extend(text_vertices);

    // Staging well
    output.extend(state.staging_well.layout(&state.text_renderer, layout.staging));

    // Diff view or secondary repos
    if state.diff_view.has_content() {
        output.extend(state.diff_view.layout(&state.text_renderer, layout.secondary_repos));
    } else {
        output.extend(state.secondary_repos_view.layout(&state.text_renderer, layout.secondary_repos));
    }

    let viewport = Viewport {
        offset: [0.0, 0.0],
        extent: [extent[0] as f32, extent[1] as f32],
        depth_range: 0.0..=1.0,
    };

    // Acquire image
    let (image_index, _, acquire_future) = acquire_next_image(state.surface.swapchain.clone(), None)
        .map_err(Validated::unwrap)
        .context("Failed to acquire image")?;

    // Build command buffer
    let mut builder = AutoCommandBufferBuilder::primary(
        state.ctx.command_buffer_allocator.clone(),
        state.ctx.queue.queue_family_index(),
        CommandBufferUsage::OneTimeSubmit,
    )
    .context("Failed to create command buffer")?;

    let bg_color = [0.051f32, 0.051, 0.051, 1.0]; // #0d0d0d

    builder
        .begin_render_pass(
            RenderPassBeginInfo {
                clear_values: vec![Some(bg_color.into())],
                ..RenderPassBeginInfo::framebuffer(
                    state.surface.framebuffers[image_index as usize].clone(),
                )
            },
            Default::default(),
        )
        .context("Failed to begin render pass")?;

    // Draw splines first (background)
    if !output.spline_vertices.is_empty() {
        let spline_buffer = state.spline_renderer.create_vertex_buffer(output.spline_vertices)?;
        state.spline_renderer.draw(&mut builder, spline_buffer, viewport.clone())?;
    }

    // Draw text on top
    if !output.text_vertices.is_empty() {
        let vertex_buffer = state.text_renderer.create_vertex_buffer(output.text_vertices)?;
        state.text_renderer.draw(&mut builder, vertex_buffer, viewport)?;
    }

    builder
        .end_render_pass(Default::default())
        .context("Failed to end render pass")?;

    // Capture to buffer
    let capture = capture_to_buffer(
        &mut builder,
        state.ctx.memory_allocator.clone(),
        state.surface.images[image_index as usize].clone(),
        state.surface.image_format(),
    )?;

    let command_buffer = builder.build().context("Failed to build command buffer")?;

    // Execute and wait
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

/// Capture a screenshot at a specific resolution using offscreen rendering
fn capture_screenshot_offscreen(
    state: &mut RenderState,
    commits: &[CommitInfo],
    width: u32,
    height: u32,
) -> Result<image::RgbaImage> {
    state.previous_frame_end.as_mut().unwrap().cleanup_finished();

    // Create offscreen render target with specified dimensions
    let offscreen = OffscreenTarget::new(
        &state.ctx,
        state.surface.render_pass.clone(),
        width,
        height,
        state.surface.image_format(),
    )?;

    // Build UI at the specified dimensions
    let extent = offscreen.extent();
    let screen_bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
    let layout = ScreenLayout::compute_with_gap(screen_bounds, 4.0, state.scale_factor as f32);

    // Collect all vertices
    let mut output = WidgetOutput::new();

    // Header bar
    output.extend(state.header_bar.layout(&state.text_renderer, layout.header));

    // Branch sidebar
    output.extend(state.branch_sidebar.layout(&state.text_renderer, layout.sidebar));

    // Commit graph
    let spline_vertices = state.commit_graph_view.layout_splines(&state.text_renderer, commits, layout.graph);
    let (text_vertices, pill_vertices) = state.commit_graph_view.layout_text(&state.text_renderer, commits, layout.graph);
    output.spline_vertices.extend(spline_vertices);
    output.spline_vertices.extend(pill_vertices);
    output.text_vertices.extend(text_vertices);

    // Staging well
    output.extend(state.staging_well.layout(&state.text_renderer, layout.staging));

    // Diff view or secondary repos
    if state.diff_view.has_content() {
        output.extend(state.diff_view.layout(&state.text_renderer, layout.secondary_repos));
    } else {
        output.extend(state.secondary_repos_view.layout(&state.text_renderer, layout.secondary_repos));
    }

    let viewport = Viewport {
        offset: [0.0, 0.0],
        extent: [extent[0] as f32, extent[1] as f32],
        depth_range: 0.0..=1.0,
    };

    // Build command buffer - no swapchain acquire needed for offscreen
    let mut builder = AutoCommandBufferBuilder::primary(
        state.ctx.command_buffer_allocator.clone(),
        state.ctx.queue.queue_family_index(),
        CommandBufferUsage::OneTimeSubmit,
    )
    .context("Failed to create command buffer")?;

    let bg_color = [0.051f32, 0.051, 0.051, 1.0]; // #0d0d0d

    builder
        .begin_render_pass(
            RenderPassBeginInfo {
                clear_values: vec![Some(bg_color.into())],
                ..RenderPassBeginInfo::framebuffer(offscreen.framebuffer.clone())
            },
            Default::default(),
        )
        .context("Failed to begin render pass")?;

    // Draw splines first (background)
    if !output.spline_vertices.is_empty() {
        let spline_buffer = state.spline_renderer.create_vertex_buffer(output.spline_vertices)?;
        state.spline_renderer.draw(&mut builder, spline_buffer, viewport.clone())?;
    }

    // Draw text on top
    if !output.text_vertices.is_empty() {
        let vertex_buffer = state.text_renderer.create_vertex_buffer(output.text_vertices)?;
        state.text_renderer.draw(&mut builder, vertex_buffer, viewport)?;
    }

    builder
        .end_render_pass(Default::default())
        .context("Failed to end render pass")?;

    // Capture to buffer from offscreen image
    let capture = capture_to_buffer(
        &mut builder,
        state.ctx.memory_allocator.clone(),
        offscreen.image.clone(),
        offscreen.format,
    )?;

    let command_buffer = builder.build().context("Failed to build command buffer")?;

    // Execute and wait - simpler than swapchain path, no acquire future needed
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
