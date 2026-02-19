use anyhow::{Context, Result};
use std::sync::Arc;
use vulkano::{
    format::Format,
    image::{Image, ImageCreateInfo, ImageType, ImageUsage, SampleCount, view::ImageView},
    memory::allocator::{AllocationCreateInfo, MemoryTypeFilter},
    render_pass::{Framebuffer, FramebufferCreateInfo, RenderPass},
    swapchain::{Surface, Swapchain, SwapchainCreateInfo},
};
use winit::window::Window;

use super::VulkanContext;

/// Manages the swapchain and associated framebuffers
pub struct SurfaceManager {
    pub swapchain: Arc<Swapchain>,
    pub images: Vec<Arc<Image>>,
    pub msaa_views: Vec<Arc<ImageView>>,
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
        let msaa_views = Self::create_msaa_images(ctx, &images)?;
        let framebuffers = Self::create_framebuffers(&images, &msaa_views, &render_pass)?;

        Ok(Self {
            swapchain,
            images,
            msaa_views,
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
                image_usage: ImageUsage::COLOR_ATTACHMENT
                    | ImageUsage::TRANSFER_SRC
                    | ImageUsage::TRANSFER_DST,
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

    /// Create MSAA images matching swapchain images
    fn create_msaa_images(
        ctx: &VulkanContext,
        swapchain_images: &[Arc<Image>],
    ) -> Result<Vec<Arc<ImageView>>> {
        swapchain_images
            .iter()
            .map(|img| {
                let extent = img.extent();
                let format = img.format();
                let msaa_image = Image::new(
                    ctx.memory_allocator.clone(),
                    ImageCreateInfo {
                        image_type: ImageType::Dim2d,
                        format,
                        extent,
                        samples: SampleCount::Sample4,
                        usage: ImageUsage::COLOR_ATTACHMENT | ImageUsage::TRANSIENT_ATTACHMENT,
                        ..Default::default()
                    },
                    AllocationCreateInfo {
                        memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
                        ..Default::default()
                    },
                )
                .context("Failed to create MSAA image")?;
                ImageView::new_default(msaa_image).context("Failed to create MSAA image view")
            })
            .collect()
    }

    /// Create framebuffers for swapchain images with MSAA
    fn create_framebuffers(
        images: &[Arc<Image>],
        msaa_views: &[Arc<ImageView>],
        render_pass: &Arc<RenderPass>,
    ) -> Result<Vec<Arc<Framebuffer>>> {
        images
            .iter()
            .zip(msaa_views.iter())
            .map(|(image, msaa_view)| {
                let resolve_view =
                    ImageView::new_default(image.clone()).context("Failed to create image view")?;
                Framebuffer::new(
                    render_pass.clone(),
                    FramebufferCreateInfo {
                        attachments: vec![msaa_view.clone(), resolve_view],
                        ..Default::default()
                    },
                )
                .context("Failed to create framebuffer")
            })
            .collect()
    }

    /// Recreate the swapchain (e.g., after resize)
    pub fn recreate(
        &mut self,
        ctx: &VulkanContext,
        size: winit::dpi::PhysicalSize<u32>,
    ) -> Result<()> {
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
        self.msaa_views = Self::create_msaa_images(ctx, &self.images)?;
        self.framebuffers =
            Self::create_framebuffers(&self.images, &self.msaa_views, &self.render_pass)?;
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
