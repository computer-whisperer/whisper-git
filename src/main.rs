mod git;
mod text;

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;

use crate::git::{CommitInfo, GitRepo};
use crate::text::TextRenderer;

/// Convert linear color value to sRGB
fn linear_to_srgb(linear: f32) -> f32 {
    if linear <= 0.0031308 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}
use vulkano::{
    buffer::{Buffer, BufferCreateInfo, BufferUsage},
    command_buffer::{
        allocator::StandardCommandBufferAllocator, AutoCommandBufferBuilder, CommandBufferUsage,
        CopyImageToBufferInfo, RenderPassBeginInfo,
    },
    device::{
        physical::PhysicalDeviceType, Device, DeviceCreateInfo, DeviceExtensions, Queue,
        QueueCreateInfo, QueueFlags,
    },
    format::Format,
    image::{view::ImageView, Image, ImageUsage},
    instance::{Instance, InstanceCreateInfo},
    memory::allocator::{AllocationCreateInfo, MemoryTypeFilter, StandardMemoryAllocator},
    render_pass::{Framebuffer, FramebufferCreateInfo, RenderPass},
    swapchain::{
        acquire_next_image, Surface, Swapchain, SwapchainCreateInfo, SwapchainPresentInfo,
    },
    sync::{self, GpuFuture},
    Validated, VulkanError, VulkanLibrary,
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

/// CLI arguments for headless/screenshot mode
#[derive(Default)]
struct CliArgs {
    /// Path to save screenshot (enables screenshot mode)
    screenshot: Option<PathBuf>,
    /// View/page to navigate to before screenshot
    view: Option<String>,
    /// Repository path to open
    repo: Option<PathBuf>,
}

fn parse_args() -> CliArgs {
    let mut args = CliArgs::default();
    let mut iter = std::env::args().skip(1);

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--screenshot" => {
                args.screenshot = iter.next().map(PathBuf::from);
            }
            "--view" => {
                args.view = iter.next();
            }
            "--repo" => {
                args.repo = iter.next().map(PathBuf::from);
            }
            other if !other.starts_with('-') => {
                // Positional arg = repo path
                args.repo = Some(PathBuf::from(other));
            }
            _ => {}
        }
    }

    args
}

fn main() -> Result<()> {
    let cli_args = parse_args();

    let event_loop = EventLoop::new().context("Failed to create event loop")?;
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::new(&event_loop, cli_args)?;

    event_loop.run_app(&mut app).context("Event loop error")?;

    Ok(())
}

struct App {
    instance: Arc<Instance>,
    cli_args: CliArgs,
    renderer: Option<Renderer>,
    repo: Option<GitRepo>,
    commits: Vec<CommitInfo>,
}

struct Renderer {
    window: Arc<Window>,
    device: Arc<Device>,
    queue: Arc<Queue>,
    swapchain: Arc<Swapchain>,
    images: Vec<Arc<Image>>,
    render_pass: Arc<RenderPass>,
    framebuffers: Vec<Arc<Framebuffer>>,
    memory_allocator: Arc<StandardMemoryAllocator>,
    command_buffer_allocator: Arc<StandardCommandBufferAllocator>,
    recreate_swapchain: bool,
    previous_frame_end: Option<Box<dyn GpuFuture>>,
    frame_count: u32,
    text_renderer: TextRenderer,
}

impl App {
    fn new(event_loop: &EventLoop<()>, cli_args: CliArgs) -> Result<Self> {
        let library = VulkanLibrary::new().context("No Vulkan library found")?;

        let required_extensions = Surface::required_extensions(event_loop)
            .context("Failed to get required surface extensions")?;

        let instance = Instance::new(
            library,
            InstanceCreateInfo {
                enabled_extensions: required_extensions,
                ..Default::default()
            },
        )
        .context("Failed to create Vulkan instance")?;

        // Try to open the repository
        let repo_path = cli_args.repo.as_deref().unwrap_or(".".as_ref());
        let (repo, commits) = match GitRepo::open(repo_path) {
            Ok(repo) => {
                let commits = repo.recent_commits(20).unwrap_or_default();
                println!("Loaded {} commits from {:?}", commits.len(), repo.workdir());
                for commit in &commits {
                    println!("  {} {}", commit.short_id, commit.summary);
                }
                (Some(repo), commits)
            }
            Err(e) => {
                eprintln!("Warning: Could not open repository: {e}");
                (None, Vec::new())
            }
        };

        Ok(Self {
            instance,
            cli_args,
            renderer: None,
            repo,
            commits,
        })
    }

    fn init_renderer(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Whisper - Git Client")
                        .with_inner_size(winit::dpi::LogicalSize::new(1280, 720)),
                )
                .context("Failed to create window")?,
        );

        let surface = Surface::from_window(self.instance.clone(), window.clone())
            .context("Failed to create surface")?;

        let device_extensions = DeviceExtensions {
            khr_swapchain: true,
            ..DeviceExtensions::empty()
        };

        let (physical_device, queue_family_index) = self
            .instance
            .enumerate_physical_devices()
            .context("Failed to enumerate physical devices")?
            .filter(|p| p.supported_extensions().contains(&device_extensions))
            .filter_map(|p| {
                p.queue_family_properties()
                    .iter()
                    .enumerate()
                    .position(|(i, q)| {
                        q.queue_flags.contains(QueueFlags::GRAPHICS)
                            && p.surface_support(i as u32, &surface).unwrap_or(false)
                    })
                    .map(|i| (p, i as u32))
            })
            .min_by_key(|(p, _)| match p.properties().device_type {
                PhysicalDeviceType::DiscreteGpu => 0,
                PhysicalDeviceType::IntegratedGpu => 1,
                PhysicalDeviceType::VirtualGpu => 2,
                PhysicalDeviceType::Cpu => 3,
                PhysicalDeviceType::Other => 4,
                _ => 5,
            })
            .context("No suitable GPU found")?;

        println!(
            "Using device: {} ({:?})",
            physical_device.properties().device_name,
            physical_device.properties().device_type
        );

        let (device, mut queues) = Device::new(
            physical_device.clone(),
            DeviceCreateInfo {
                enabled_extensions: device_extensions,
                queue_create_infos: vec![QueueCreateInfo {
                    queue_family_index,
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .context("Failed to create device")?;

        let queue = queues.next().context("No queue available")?;

        let window_size = window.inner_size();

        let (swapchain, images) = {
            let surface_capabilities = physical_device
                .surface_capabilities(&surface, Default::default())
                .context("Failed to get surface capabilities")?;

            let image_format = physical_device
                .surface_formats(&surface, Default::default())
                .context("Failed to get surface formats")?[0]
                .0;

            Swapchain::new(
                device.clone(),
                surface,
                SwapchainCreateInfo {
                    min_image_count: surface_capabilities.min_image_count.max(2),
                    image_format,
                    image_extent: window_size.into(),
                    image_usage: ImageUsage::COLOR_ATTACHMENT | ImageUsage::TRANSFER_SRC,
                    composite_alpha: surface_capabilities
                        .supported_composite_alpha
                        .into_iter()
                        .next()
                        .context("No composite alpha mode supported")?,
                    ..Default::default()
                },
            )
            .context("Failed to create swapchain")?
        };

        let memory_allocator = Arc::new(StandardMemoryAllocator::new_default(device.clone()));
        let command_buffer_allocator = Arc::new(StandardCommandBufferAllocator::new(
            device.clone(),
            Default::default(),
        ));

        let render_pass = vulkano::single_pass_renderpass!(
            device.clone(),
            attachments: {
                color: {
                    format: swapchain.image_format(),
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

        let framebuffers = create_framebuffers(&images, &render_pass)?;

        // Create text renderer (uploads font atlas)
        let mut upload_builder = AutoCommandBufferBuilder::primary(
            command_buffer_allocator.clone(),
            queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .context("Failed to create upload command buffer")?;

        let text_renderer = TextRenderer::new(
            memory_allocator.clone(),
            render_pass.clone(),
            &mut upload_builder,
        )
        .context("Failed to create text renderer")?;

        // Submit font atlas upload
        let upload_buffer = upload_builder.build().context("Failed to build upload buffer")?;
        let upload_future = sync::now(device.clone())
            .then_execute(queue.clone(), upload_buffer)
            .context("Failed to execute upload")?
            .then_signal_fence_and_flush()
            .map_err(Validated::unwrap)
            .context("Failed to flush upload")?;

        upload_future.wait(None).context("Failed to wait for upload")?;

        let previous_frame_end = Some(sync::now(device.clone()).boxed());

        self.renderer = Some(Renderer {
            window,
            device,
            queue,
            swapchain,
            images,
            render_pass,
            framebuffers,
            memory_allocator,
            command_buffer_allocator,
            recreate_swapchain: false,
            previous_frame_end,
            frame_count: 0,
            text_renderer,
        });

        Ok(())
    }
}

fn create_framebuffers(
    images: &[Arc<Image>],
    render_pass: &Arc<RenderPass>,
) -> Result<Vec<Arc<Framebuffer>>> {
    images
        .iter()
        .map(|image| {
            let view =
                ImageView::new_default(image.clone()).context("Failed to create image view")?;
            Framebuffer::new(
                render_pass.clone(),
                FramebufferCreateInfo {
                    attachments: vec![view],
                    ..Default::default()
                },
            )
            .context("Failed to create framebuffer")
        })
        .collect()
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_none() {
            if let Err(e) = self.init_renderer(event_loop) {
                eprintln!("Failed to initialize renderer: {e:?}");
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
        let Some(renderer) = &mut self.renderer else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::Resized(_) => {
                renderer.recreate_swapchain = true;
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = renderer.draw(&self.commits) {
                    eprintln!("Draw error: {e:?}");
                }

                // Screenshot mode: capture after a few frames for stability
                if let Some(ref screenshot_path) = self.cli_args.screenshot {
                    if renderer.frame_count == 3 {
                        match renderer.capture_screenshot(&self.commits) {
                            Ok(img) => {
                                if let Err(e) = img.save(screenshot_path) {
                                    eprintln!("Failed to save screenshot: {e}");
                                } else {
                                    println!("Screenshot saved to: {}", screenshot_path.display());
                                }
                            }
                            Err(e) => eprintln!("Failed to capture screenshot: {e:?}"),
                        }
                        event_loop.exit();
                        return;
                    }
                }

                renderer.window.request_redraw();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(renderer) = &self.renderer {
            renderer.window.request_redraw();
        }
    }
}

impl Renderer {
    fn draw(&mut self, commits: &[CommitInfo]) -> Result<()> {
        use vulkano::pipeline::graphics::viewport::Viewport;

        self.previous_frame_end
            .as_mut()
            .unwrap()
            .cleanup_finished();

        if self.recreate_swapchain {
            self.recreate_swapchain()?;
        }

        let (image_index, suboptimal, acquire_future) =
            match acquire_next_image(self.swapchain.clone(), None).map_err(Validated::unwrap) {
                Ok(r) => r,
                Err(VulkanError::OutOfDate) => {
                    self.recreate_swapchain = true;
                    return Ok(());
                }
                Err(e) => anyhow::bail!("Failed to acquire next image: {e:?}"),
            };

        if suboptimal {
            self.recreate_swapchain = true;
        }

        // Build text vertices for commits
        let mut all_vertices = Vec::new();
        let line_height = self.text_renderer.line_height();
        let mut y = 40.0; // Start with some padding

        // Title
        all_vertices.extend(self.text_renderer.layout_text(
            "Recent Commits",
            20.0,
            y,
            [0.9, 0.9, 0.95, 1.0],
        ));
        y += line_height * 1.5;

        // Commits
        for commit in commits.iter().take(15) {
            let text = format!("{} {}", commit.short_id, commit.summary);
            // Truncate long lines
            let text = if text.len() > 80 {
                format!("{}...", &text[..77])
            } else {
                text
            };

            all_vertices.extend(self.text_renderer.layout_text(
                &text,
                20.0,
                y,
                [0.7, 0.75, 0.8, 1.0],
            ));
            y += line_height;
        }

        let extent = self.swapchain.image_extent();
        let viewport = Viewport {
            offset: [0.0, 0.0],
            extent: [extent[0] as f32, extent[1] as f32],
            depth_range: 0.0..=1.0,
        };

        let mut builder = AutoCommandBufferBuilder::primary(
            self.command_buffer_allocator.clone(),
            self.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .context("Failed to create command buffer builder")?;

        builder
            .begin_render_pass(
                RenderPassBeginInfo {
                    clear_values: vec![Some([0.02, 0.02, 0.05, 1.0].into())],
                    ..RenderPassBeginInfo::framebuffer(
                        self.framebuffers[image_index as usize].clone(),
                    )
                },
                Default::default(),
            )
            .context("Failed to begin render pass")?;

        // Draw text if we have vertices
        if !all_vertices.is_empty() {
            let vertex_buffer = self.text_renderer.create_vertex_buffer(all_vertices)?;
            self.text_renderer.draw(&mut builder, vertex_buffer, viewport)?;
        }

        builder
            .end_render_pass(Default::default())
            .context("Failed to end render pass")?;

        let command_buffer = builder.build().context("Failed to build command buffer")?;

        let future = self
            .previous_frame_end
            .take()
            .unwrap()
            .join(acquire_future)
            .then_execute(self.queue.clone(), command_buffer)
            .context("Failed to execute command buffer")?
            .then_swapchain_present(
                self.queue.clone(),
                SwapchainPresentInfo::swapchain_image_index(self.swapchain.clone(), image_index),
            )
            .then_signal_fence_and_flush();

        match future.map_err(Validated::unwrap) {
            Ok(future) => {
                self.previous_frame_end = Some(future.boxed());
            }
            Err(VulkanError::OutOfDate) => {
                self.recreate_swapchain = true;
                self.previous_frame_end = Some(sync::now(self.device.clone()).boxed());
            }
            Err(e) => {
                self.previous_frame_end = Some(sync::now(self.device.clone()).boxed());
                anyhow::bail!("Failed to flush: {e:?}");
            }
        }

        self.frame_count += 1;
        Ok(())
    }

    fn capture_screenshot(&mut self, commits: &[CommitInfo]) -> Result<image::RgbaImage> {
        use vulkano::pipeline::graphics::viewport::Viewport;

        // Wait for previous frame to complete
        self.previous_frame_end
            .as_mut()
            .unwrap()
            .cleanup_finished();

        let extent = self.swapchain.image_extent();
        let (width, height) = (extent[0], extent[1]);

        // Get bytes per pixel based on format
        let format = self.swapchain.image_format();
        let bytes_per_pixel = format.block_size() as u32;

        // Create a buffer to read the image data into
        let buffer_size = (width * height * bytes_per_pixel) as usize;
        let buf = Buffer::from_iter(
            self.memory_allocator.clone(),
            BufferCreateInfo {
                usage: BufferUsage::TRANSFER_DST,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_HOST
                    | MemoryTypeFilter::HOST_RANDOM_ACCESS,
                ..Default::default()
            },
            (0..buffer_size).map(|_| 0u8),
        )
        .context("Failed to create screenshot buffer")?;

        // Build text vertices (same as draw)
        let mut all_vertices = Vec::new();
        let line_height = self.text_renderer.line_height();
        let mut y = 40.0;

        all_vertices.extend(self.text_renderer.layout_text(
            "Recent Commits",
            20.0,
            y,
            [0.9, 0.9, 0.95, 1.0],
        ));
        y += line_height * 1.5;

        for commit in commits.iter().take(15) {
            let text = format!("{} {}", commit.short_id, commit.summary);
            let text = if text.len() > 80 {
                format!("{}...", &text[..77])
            } else {
                text
            };
            all_vertices.extend(self.text_renderer.layout_text(
                &text,
                20.0,
                y,
                [0.7, 0.75, 0.8, 1.0],
            ));
            y += line_height;
        }

        let viewport = Viewport {
            offset: [0.0, 0.0],
            extent: [width as f32, height as f32],
            depth_range: 0.0..=1.0,
        };

        // Acquire an image and render, then copy to buffer
        let (image_index, _suboptimal, acquire_future) =
            acquire_next_image(self.swapchain.clone(), None)
                .map_err(Validated::unwrap)
                .context("Failed to acquire image for screenshot")?;

        let mut builder = AutoCommandBufferBuilder::primary(
            self.command_buffer_allocator.clone(),
            self.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .context("Failed to create command buffer")?;

        // Render the frame with text
        builder
            .begin_render_pass(
                RenderPassBeginInfo {
                    clear_values: vec![Some([0.02, 0.02, 0.05, 1.0].into())],
                    ..RenderPassBeginInfo::framebuffer(
                        self.framebuffers[image_index as usize].clone(),
                    )
                },
                Default::default(),
            )
            .context("Failed to begin render pass")?;

        if !all_vertices.is_empty() {
            let vertex_buffer = self.text_renderer.create_vertex_buffer(all_vertices)?;
            self.text_renderer.draw(&mut builder, vertex_buffer, viewport)?;
        }

        builder
            .end_render_pass(Default::default())
            .context("Failed to end render pass")?;

        // Copy swapchain image to buffer
        builder
            .copy_image_to_buffer(CopyImageToBufferInfo::image_buffer(
                self.images[image_index as usize].clone(),
                buf.clone(),
            ))
            .context("Failed to copy image to buffer")?;

        let command_buffer = builder.build().context("Failed to build command buffer")?;

        // Execute and wait
        let future = self
            .previous_frame_end
            .take()
            .unwrap()
            .join(acquire_future)
            .then_execute(self.queue.clone(), command_buffer)
            .context("Failed to execute")?
            .then_signal_fence_and_flush()
            .map_err(Validated::unwrap)
            .context("Failed to flush")?;

        future.wait(None).context("Failed to wait for GPU")?;
        self.previous_frame_end = Some(sync::now(self.device.clone()).boxed());

        // Read buffer contents and convert to RGBA based on format
        let buffer_content = buf.read().context("Failed to read buffer")?;

        let rgba_data: Vec<u8> = match format {
            // 16-bit float formats (common on AMD)
            Format::R16G16B16A16_SFLOAT => {
                use half::f16;
                buffer_content
                    .chunks(8) // 4 channels x 2 bytes each
                    .flat_map(|pixel| {
                        let r = f16::from_le_bytes([pixel[0], pixel[1]]).to_f32();
                        let g = f16::from_le_bytes([pixel[2], pixel[3]]).to_f32();
                        let b = f16::from_le_bytes([pixel[4], pixel[5]]).to_f32();
                        let a = f16::from_le_bytes([pixel[6], pixel[7]]).to_f32();
                        // Convert linear to sRGB and then to 8-bit
                        [
                            (linear_to_srgb(r) * 255.0) as u8,
                            (linear_to_srgb(g) * 255.0) as u8,
                            (linear_to_srgb(b) * 255.0) as u8,
                            (a * 255.0) as u8,
                        ]
                    })
                    .collect()
            }
            // BGRA 8-bit formats
            Format::B8G8R8A8_SRGB | Format::B8G8R8A8_UNORM => {
                buffer_content
                    .chunks(4)
                    .flat_map(|bgra| [bgra[2], bgra[1], bgra[0], bgra[3]])
                    .collect()
            }
            // RGBA 8-bit formats
            _ => buffer_content.to_vec(),
        };

        let img = image::RgbaImage::from_raw(width, height, rgba_data)
            .context("Failed to create image from buffer")?;

        Ok(img)
    }

    fn recreate_swapchain(&mut self) -> Result<()> {
        let window_size = self.window.inner_size();
        if window_size.width == 0 || window_size.height == 0 {
            return Ok(());
        }

        let (new_swapchain, new_images) = self
            .swapchain
            .recreate(SwapchainCreateInfo {
                image_extent: window_size.into(),
                ..self.swapchain.create_info()
            })
            .context("Failed to recreate swapchain")?;

        self.swapchain = new_swapchain;
        self.images = new_images;
        self.framebuffers = create_framebuffers(&self.images, &self.render_pass)?;
        self.recreate_swapchain = false;

        Ok(())
    }
}
