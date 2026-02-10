use anyhow::{Context, Result};
use std::sync::Arc;
use vulkano::{
    format::Format,
    image::{view::ImageView, Image, ImageCreateInfo, ImageType, ImageUsage, SampleCount},
    memory::allocator::{AllocationCreateInfo, MemoryTypeFilter},
    render_pass::{Framebuffer, FramebufferCreateInfo, RenderPass},
};

use super::VulkanContext;

/// Offscreen render target for controlled-size rendering with MSAA 4x
///
/// This bypasses the swapchain entirely, allowing rendering at arbitrary
/// dimensions independent of window size. Uses MSAA + resolve for AA.
pub struct OffscreenTarget {
    /// The resolve target image (single-sampled) — read this for screenshot capture
    pub image: Arc<Image>,
    pub framebuffer: Arc<Framebuffer>,
    pub format: Format,
    pub width: u32,
    pub height: u32,
}

impl OffscreenTarget {
    /// Create a new offscreen render target with specified dimensions
    pub fn new(
        ctx: &VulkanContext,
        render_pass: Arc<RenderPass>,
        width: u32,
        height: u32,
        format: Format,
    ) -> Result<Self> {
        // Create MSAA image (multisampled, transient — discarded after resolve)
        let msaa_image = Image::new(
            ctx.memory_allocator.clone(),
            ImageCreateInfo {
                image_type: ImageType::Dim2d,
                format,
                extent: [width, height, 1],
                samples: SampleCount::Sample4,
                usage: ImageUsage::COLOR_ATTACHMENT | ImageUsage::TRANSIENT_ATTACHMENT,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
                ..Default::default()
            },
        )
        .context("Failed to create offscreen MSAA image")?;

        let msaa_view = ImageView::new_default(msaa_image)
            .context("Failed to create offscreen MSAA image view")?;

        // Create resolve target image (single-sampled — read back for screenshot)
        let resolve_image = Image::new(
            ctx.memory_allocator.clone(),
            ImageCreateInfo {
                image_type: ImageType::Dim2d,
                format,
                extent: [width, height, 1],
                usage: ImageUsage::COLOR_ATTACHMENT | ImageUsage::TRANSFER_SRC | ImageUsage::TRANSFER_DST,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
                ..Default::default()
            },
        )
        .context("Failed to create offscreen resolve image")?;

        let resolve_view = ImageView::new_default(resolve_image.clone())
            .context("Failed to create offscreen resolve image view")?;

        // Create framebuffer with both MSAA and resolve attachments
        let framebuffer = Framebuffer::new(
            render_pass,
            FramebufferCreateInfo {
                attachments: vec![msaa_view, resolve_view],
                ..Default::default()
            },
        )
        .context("Failed to create offscreen framebuffer")?;

        Ok(Self {
            image: resolve_image,
            framebuffer,
            format,
            width,
            height,
        })
    }

    /// Get the extent as [width, height] array
    pub fn extent(&self) -> [u32; 2] {
        [self.width, self.height]
    }
}
