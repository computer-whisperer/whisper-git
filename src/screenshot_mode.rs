//! Headless screenshot mode: drives the same `aetna_vulkano::Runner`
//! used in the windowed host into an offscreen framebuffer, captures
//! the result, and writes a PNG. No surface, no swapchain.
//!
//! Coexists with the `aetna-core` bundle dump (`bin/dump_bundles`).
//! Bundle dumps are CPU-only and verify layout; this path renders
//! through the real GPU pipeline and verifies shader output.

use std::path::Path;
use std::sync::Arc;

use aetna_core::{App, BuildCx, Rect};
use aetna_vulkano::Runner;
use anyhow::{Context, Result};
use vulkano::{
    Validated, VulkanLibrary,
    buffer::{Buffer, BufferCreateInfo, BufferUsage, Subbuffer},
    command_buffer::{
        AutoCommandBufferBuilder, CommandBufferUsage, CopyImageToBufferInfo,
        PrimaryAutoCommandBuffer, allocator::StandardCommandBufferAllocator,
    },
    device::{
        Device, DeviceCreateInfo, DeviceExtensions, Queue, QueueCreateInfo, QueueFlags,
        physical::PhysicalDeviceType,
    },
    format::Format,
    image::{Image, ImageCreateInfo, ImageType, ImageUsage, view::ImageView},
    instance::{Instance, InstanceCreateFlags, InstanceCreateInfo},
    memory::allocator::{AllocationCreateInfo, MemoryTypeFilter, StandardMemoryAllocator},
    render_pass::{Framebuffer, FramebufferCreateInfo},
    sync::{self, GpuFuture},
};

/// Format used for offscreen rendering. RGBA8 UNORM is broadly supported
/// and matches what `image::RgbaImage` expects on read-back.
const OFFSCREEN_FORMAT: Format = Format::R8G8B8A8_UNORM;

pub fn run<A: App + 'static>(
    out_path: &Path,
    width: u32,
    height: u32,
    scale_factor: f32,
    mut app: A,
) -> Result<()> {
    let library = VulkanLibrary::new().context("no Vulkan library")?;
    let instance = Instance::new(
        library,
        InstanceCreateInfo {
            flags: InstanceCreateFlags::ENUMERATE_PORTABILITY,
            ..Default::default()
        },
    )
    .context("create instance")?;

    let (device, queue) = select_headless_device(&instance)?;
    let mem_alloc = Arc::new(StandardMemoryAllocator::new_default(device.clone()));
    let cmd_alloc = Arc::new(StandardCommandBufferAllocator::new(
        device.clone(),
        Default::default(),
    ));

    let mut runner = Runner::new(device.clone(), queue.clone(), OFFSCREEN_FORMAT);
    runner.set_theme(app.theme());
    runner.set_surface_size(width, height);
    for s in app.shaders() {
        runner.register_shader_with(s.name, s.wgsl, s.samples_backdrop, s.samples_time);
    }

    // Offscreen target — single image, no swapchain. TRANSFER_SRC so we
    // can copy out for capture; TRANSFER_DST for backdrop snapshot copies
    // (aetna-vulkano performs them when a backdrop-sampling shader is
    // present).
    let target_image = Image::new(
        mem_alloc.clone(),
        ImageCreateInfo {
            image_type: ImageType::Dim2d,
            format: OFFSCREEN_FORMAT,
            extent: [width, height, 1],
            usage: ImageUsage::COLOR_ATTACHMENT
                | ImageUsage::TRANSFER_SRC
                | ImageUsage::TRANSFER_DST,
            ..Default::default()
        },
        AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
            ..Default::default()
        },
    )
    .context("create offscreen image")?;
    let target_view =
        ImageView::new_default(target_image.clone()).context("offscreen image view")?;
    let framebuffer = Framebuffer::new(
        runner.render_pass().clone(),
        FramebufferCreateInfo {
            attachments: vec![target_view],
            ..Default::default()
        },
    )
    .context("offscreen framebuffer")?;

    // Build + prepare
    app.before_build();
    let theme = app.theme();
    let cx = BuildCx::new(&theme);
    let mut tree = app.build(&cx);
    runner.set_theme(theme.clone());
    let viewport = Rect::new(
        0.0,
        0.0,
        width as f32 / scale_factor,
        height as f32 / scale_factor,
    );
    runner.prepare(&mut tree, viewport, scale_factor);

    // Record + submit
    let mut builder = AutoCommandBufferBuilder::primary(
        cmd_alloc.clone(),
        queue.queue_family_index(),
        CommandBufferUsage::OneTimeSubmit,
    )
    .context("command builder")?;
    runner.render(
        &mut builder,
        framebuffer,
        target_image.clone(),
        clear_color(&theme),
    );
    let capture = capture_to_buffer(&mut builder, mem_alloc, target_image)?;
    let cb = builder.build().context("build cmd")?;

    sync::now(device.clone())
        .then_execute(queue, cb)
        .context("execute cmd")?
        .then_signal_fence_and_flush()
        .map_err(Validated::unwrap)
        .context("flush")?
        .wait(None)
        .context("wait")?;

    let img = capture.to_image()?;
    img.save(out_path).context("save png")?;
    Ok(())
}

fn select_headless_device(instance: &Arc<Instance>) -> Result<(Arc<Device>, Arc<Queue>)> {
    let device_extensions = DeviceExtensions::empty();
    let gpu_pref = std::env::var("WHISPER_GPU").ok();

    let (physical_device, queue_family_index) = instance
        .enumerate_physical_devices()
        .context("enumerate physical devices")?
        .filter_map(|p| {
            p.queue_family_properties()
                .iter()
                .enumerate()
                .position(|(_, q)| q.queue_flags.contains(QueueFlags::GRAPHICS))
                .map(|i| (p, i as u32))
        })
        .min_by_key(|(p, _)| {
            let dev_type = p.properties().device_type;
            let dev_name = p.properties().device_name.to_lowercase();
            match gpu_pref.as_deref() {
                Some("integrated") => match dev_type {
                    PhysicalDeviceType::IntegratedGpu => 0,
                    PhysicalDeviceType::DiscreteGpu => 1,
                    _ => 2,
                },
                Some("discrete") | None => match dev_type {
                    PhysicalDeviceType::DiscreteGpu => 0,
                    PhysicalDeviceType::IntegratedGpu => 1,
                    _ => 2,
                },
                Some(name) => {
                    if dev_name.contains(&name.to_lowercase()) {
                        0
                    } else {
                        1
                    }
                }
            }
        })
        .context("no suitable GPU found")?;

    let (device, mut queues) = Device::new(
        physical_device,
        DeviceCreateInfo {
            enabled_extensions: device_extensions,
            enabled_features: aetna_vulkano::required_device_features(),
            queue_create_infos: vec![QueueCreateInfo {
                queue_family_index,
                ..Default::default()
            }],
            ..Default::default()
        },
    )
    .context("create device")?;
    let queue = queues.next().context("no queue")?;
    Ok((device, queue))
}

struct Capture {
    buffer: Subbuffer<[u8]>,
    width: u32,
    height: u32,
    format: Format,
}

impl Capture {
    fn to_image(&self) -> Result<image::RgbaImage> {
        let bytes = self.buffer.read().context("read capture buffer")?;
        let rgba: Vec<u8> = match self.format {
            Format::R8G8B8A8_UNORM | Format::R8G8B8A8_SRGB => bytes.to_vec(),
            Format::B8G8R8A8_UNORM | Format::B8G8R8A8_SRGB => bytes
                .chunks(4)
                .flat_map(|p| [p[2], p[1], p[0], p[3]])
                .collect(),
            other => anyhow::bail!("unsupported capture format: {other:?}"),
        };
        image::RgbaImage::from_raw(self.width, self.height, rgba).context("RgbaImage::from_raw")
    }
}

fn capture_to_buffer(
    builder: &mut AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
    mem_alloc: Arc<StandardMemoryAllocator>,
    image: Arc<Image>,
) -> Result<Capture> {
    let extent = image.extent();
    let (width, height) = (extent[0], extent[1]);
    let bytes_per_pixel = image.format().block_size() as u32;
    let buffer_size = (width * height * bytes_per_pixel) as usize;

    let buffer: Subbuffer<[u8]> = Buffer::from_iter(
        mem_alloc,
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
    .context("capture buffer")?;

    builder
        .copy_image_to_buffer(CopyImageToBufferInfo::image_buffer(
            image.clone(),
            buffer.clone(),
        ))
        .context("copy image to buffer")?;

    Ok(Capture {
        buffer,
        width,
        height,
        format: image.format(),
    })
}

fn clear_color(theme: &aetna_core::Theme) -> [f32; 4] {
    let c = theme.resolve(aetna_core::tokens::BACKGROUND);
    [
        srgb_to_linear(c.r as f32 / 255.0),
        srgb_to_linear(c.g as f32 / 255.0),
        srgb_to_linear(c.b as f32 / 255.0),
        c.a as f32 / 255.0,
    ]
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}
