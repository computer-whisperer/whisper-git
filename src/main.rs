mod git;
mod renderer;
mod ui;
mod views;

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
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

use crate::git::{CommitInfo, GitRepo};
use crate::renderer::{capture_to_buffer, SurfaceManager, VulkanContext};
use crate::ui::{Rect, TextRenderer};
use crate::views::CommitListView;

// ============================================================================
// CLI
// ============================================================================

#[derive(Default)]
struct CliArgs {
    screenshot: Option<PathBuf>,
    view: Option<String>,
    repo: Option<PathBuf>,
}

fn parse_args() -> CliArgs {
    let mut args = CliArgs::default();
    let mut iter = std::env::args().skip(1);

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--screenshot" => args.screenshot = iter.next().map(PathBuf::from),
            "--view" => args.view = iter.next(),
            "--repo" => args.repo = iter.next().map(PathBuf::from),
            other if !other.starts_with('-') => args.repo = Some(PathBuf::from(other)),
            _ => {}
        }
    }

    args
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
    commits: Vec<CommitInfo>,
    state: Option<AppState>,
}

/// Initialized state (after window creation)
struct AppState {
    window: Arc<Window>,
    ctx: VulkanContext,
    surface: SurfaceManager,
    text_renderer: TextRenderer,
    commit_view: CommitListView,
    previous_frame_end: Option<Box<dyn GpuFuture>>,
    frame_count: u32,
}

impl App {
    fn new(cli_args: CliArgs) -> Result<Self> {
        // Load commits from repo
        let repo_path = cli_args.repo.as_deref().unwrap_or(".".as_ref());
        let commits = match GitRepo::open(repo_path) {
            Ok(repo) => {
                let commits = repo.recent_commits(20).unwrap_or_default();
                println!("Loaded {} commits from {:?}", commits.len(), repo.workdir());
                for commit in &commits {
                    println!("  {} {}", commit.short_id, commit.summary);
                }
                commits
            }
            Err(e) => {
                eprintln!("Warning: Could not open repository: {e}");
                Vec::new()
            }
        };

        Ok(Self {
            cli_args,
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
                        .with_title("Whisper - Git Client")
                        .with_inner_size(winit::dpi::LogicalSize::new(1280, 720)),
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

        let text_renderer = TextRenderer::new(
            ctx.memory_allocator.clone(),
            render_pass.clone(),
            &mut upload_builder,
        )
        .context("Failed to create text renderer")?;

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

        self.state = Some(AppState {
            window,
            ctx,
            surface: surface_mgr,
            text_renderer,
            commit_view: CommitListView::new(),
            previous_frame_end,
            frame_count: 0,
        });

        Ok(())
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

            WindowEvent::RedrawRequested => {
                if let Err(e) = draw_frame(state, &self.commits) {
                    eprintln!("Draw error: {e:?}");
                }

                // Screenshot mode
                if let Some(ref path) = self.cli_args.screenshot {
                    if state.frame_count == 3 {
                        match capture_screenshot(state, &self.commits) {
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

            _ => {}
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

fn draw_frame(state: &mut AppState, commits: &[CommitInfo]) -> Result<()> {
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
    let bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
    let vertices = state.commit_view.layout(&state.text_renderer, commits, bounds);

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

    builder
        .begin_render_pass(
            RenderPassBeginInfo {
                clear_values: vec![Some([0.02, 0.02, 0.05, 1.0].into())],
                ..RenderPassBeginInfo::framebuffer(
                    state.surface.framebuffers[image_index as usize].clone(),
                )
            },
            Default::default(),
        )
        .context("Failed to begin render pass")?;

    // Draw text
    if !vertices.is_empty() {
        let vertex_buffer = state.text_renderer.create_vertex_buffer(vertices)?;
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

fn capture_screenshot(state: &mut AppState, commits: &[CommitInfo]) -> Result<image::RgbaImage> {
    state.previous_frame_end.as_mut().unwrap().cleanup_finished();

    // Build UI
    let extent = state.surface.extent();
    let bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
    let vertices = state.commit_view.layout(&state.text_renderer, commits, bounds);

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

    builder
        .begin_render_pass(
            RenderPassBeginInfo {
                clear_values: vec![Some([0.02, 0.02, 0.05, 1.0].into())],
                ..RenderPassBeginInfo::framebuffer(
                    state.surface.framebuffers[image_index as usize].clone(),
                )
            },
            Default::default(),
        )
        .context("Failed to begin render pass")?;

    if !vertices.is_empty() {
        let vertex_buffer = state.text_renderer.create_vertex_buffer(vertices)?;
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
