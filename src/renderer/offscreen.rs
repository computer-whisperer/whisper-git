use anyhow::{Context, Result};
use std::sync::Arc;
use vulkano::{
    format::Format,
    image::{view::ImageView, Image, ImageCreateInfo, ImageType, ImageUsage},
    memory::allocator::{AllocationCreateInfo, MemoryTypeFilter},
    render_pass::{Framebuffer, FramebufferCreateInfo, RenderPass},
};

use super::VulkanContext;

/// Offscreen render target for controlled-size rendering
///
/// This bypasses the swapchain entirely, allowing rendering at arbitrary
/// dimensions independent of window size.
pub struct OffscreenTarget {
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
        // Create the image with COLOR_ATTACHMENT for rendering and TRANSFER_SRC for capture
        let image = Image::new(
            ctx.memory_allocator.clone(),
            ImageCreateInfo {
                image_type: ImageType::Dim2d,
                format,
                extent: [width, height, 1],
                usage: ImageUsage::COLOR_ATTACHMENT | ImageUsage::TRANSFER_SRC,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
                ..Default::default()
            },
        )
        .context("Failed to create offscreen image")?;

        // Create image view for framebuffer attachment
        let image_view = ImageView::new_default(image.clone())
            .context("Failed to create offscreen image view")?;

        // Create framebuffer
        let framebuffer = Framebuffer::new(
            render_pass,
            FramebufferCreateInfo {
                attachments: vec![image_view],
                ..Default::default()
            },
        )
        .context("Failed to create offscreen framebuffer")?;

        Ok(Self {
            image,
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
