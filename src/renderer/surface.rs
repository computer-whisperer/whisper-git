use anyhow::{Context, Result};
use std::sync::Arc;
use vulkano::{
    format::Format,
    image::{view::ImageView, Image, ImageUsage},
    render_pass::{Framebuffer, FramebufferCreateInfo, RenderPass},
    swapchain::{Surface, Swapchain, SwapchainCreateInfo},
};
use winit::window::Window;

use super::VulkanContext;

/// Manages the swapchain and associated framebuffers
#[allow(dead_code)]
pub struct SurfaceManager {
    pub surface: Arc<Surface>,
    pub swapchain: Arc<Swapchain>,
    pub images: Vec<Arc<Image>>,
    pub framebuffers: Vec<Arc<Framebuffer>>,
    pub render_pass: Arc<RenderPass>,
    pub needs_recreate: bool,
}

impl SurfaceManager {
    /// Create a new surface manager
    pub fn new(
        ctx: &VulkanContext,
        window: Arc<Window>,
        render_pass: Arc<RenderPass>,
    ) -> Result<Self> {
        let surface = Surface::from_window(ctx.instance.clone(), window.clone())
            .context("Failed to create surface")?;

        let (swapchain, images) = Self::create_swapchain(ctx, &surface, window.inner_size())?;
        let framebuffers = Self::create_framebuffers(&images, &render_pass)?;

        Ok(Self {
            surface,
            swapchain,
            images,
            framebuffers,
            render_pass,
            needs_recreate: false,
        })
    }

    /// Create swapchain for a surface
    fn create_swapchain(
        ctx: &VulkanContext,
        surface: &Arc<Surface>,
        size: winit::dpi::PhysicalSize<u32>,
    ) -> Result<(Arc<Swapchain>, Vec<Arc<Image>>)> {
        let physical_device = ctx.device.physical_device();

        let surface_capabilities = physical_device
            .surface_capabilities(surface, Default::default())
            .context("Failed to get surface capabilities")?;

        let image_format = physical_device
            .surface_formats(surface, Default::default())
            .context("Failed to get surface formats")?[0]
            .0;

        Swapchain::new(
            ctx.device.clone(),
            surface.clone(),
            SwapchainCreateInfo {
                min_image_count: surface_capabilities.min_image_count.max(2),
                image_format,
                image_extent: size.into(),
                image_usage: ImageUsage::COLOR_ATTACHMENT | ImageUsage::TRANSFER_SRC,
                composite_alpha: surface_capabilities
                    .supported_composite_alpha
                    .into_iter()
                    .next()
                    .context("No composite alpha mode supported")?,
                ..Default::default()
            },
        )
        .context("Failed to create swapchain")
    }

    /// Create framebuffers for swapchain images
    fn create_framebuffers(
        images: &[Arc<Image>],
        render_pass: &Arc<RenderPass>,
    ) -> Result<Vec<Arc<Framebuffer>>> {
        images
            .iter()
            .map(|image| {
                let view = ImageView::new_default(image.clone())
                    .context("Failed to create image view")?;
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

    /// Recreate the swapchain (e.g., after resize)
    pub fn recreate(&mut self, _ctx: &VulkanContext, size: winit::dpi::PhysicalSize<u32>) -> Result<()> {
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }

        let (new_swapchain, new_images) = self
            .swapchain
            .recreate(SwapchainCreateInfo {
                image_extent: size.into(),
                ..self.swapchain.create_info()
            })
            .context("Failed to recreate swapchain")?;

        self.swapchain = new_swapchain;
        self.images = new_images;
        self.framebuffers = Self::create_framebuffers(&self.images, &self.render_pass)?;
        self.needs_recreate = false;

        Ok(())
    }

    /// Get the swapchain image format
    pub fn image_format(&self) -> Format {
        self.swapchain.image_format()
    }

    /// Get the swapchain extent
    pub fn extent(&self) -> [u32; 2] {
        self.swapchain.image_extent()
    }
}
