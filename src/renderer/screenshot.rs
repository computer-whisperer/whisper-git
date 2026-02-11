use anyhow::{Context, Result};
use vulkano::{
    buffer::{Buffer, BufferCreateInfo, BufferUsage},
    command_buffer::{AutoCommandBufferBuilder, CopyImageToBufferInfo, PrimaryAutoCommandBuffer},
    format::Format,
    image::Image,
    memory::allocator::{AllocationCreateInfo, MemoryTypeFilter, StandardMemoryAllocator},
};
use std::sync::Arc;

/// Capture a screenshot from a swapchain image
pub fn capture_to_buffer(
    builder: &mut AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
    memory_allocator: Arc<StandardMemoryAllocator>,
    image: Arc<Image>,
    format: Format,
) -> Result<CaptureBuffer> {
    let extent = image.extent();
    let (width, height) = (extent[0], extent[1]);
    let bytes_per_pixel = format.block_size() as u32;
    let buffer_size = (width * height * bytes_per_pixel) as usize;

    let buffer = Buffer::from_iter(
        memory_allocator,
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

    builder
        .copy_image_to_buffer(CopyImageToBufferInfo::image_buffer(image, buffer.clone()))
        .context("Failed to copy image to buffer")?;

    Ok(CaptureBuffer {
        buffer,
        width,
        height,
        format,
    })
}

/// Buffer containing captured screenshot data
pub struct CaptureBuffer {
    buffer: vulkano::buffer::Subbuffer<[u8]>,
    width: u32,
    height: u32,
    format: Format,
}

impl CaptureBuffer {
    /// Convert to an RGBA image (call after GPU work is complete)
    pub fn to_image(&self) -> Result<image::RgbaImage> {
        let buffer_content = self.buffer.read().context("Failed to read buffer")?;

        let rgba_data: Vec<u8> = match self.format {
            // 16-bit float formats (common on AMD)
            // The application writes sRGB color values directly (all color constants
            // are perceptual/sRGB), so the framebuffer already contains sRGB data.
            // No linear-to-sRGB conversion needed - just clamp and quantize.
            Format::R16G16B16A16_SFLOAT => {
                use half::f16;
                buffer_content
                    .chunks(8)
                    .flat_map(|pixel| {
                        let r = f16::from_le_bytes([pixel[0], pixel[1]]).to_f32();
                        let g = f16::from_le_bytes([pixel[2], pixel[3]]).to_f32();
                        let b = f16::from_le_bytes([pixel[4], pixel[5]]).to_f32();
                        let a = f16::from_le_bytes([pixel[6], pixel[7]]).to_f32();
                        [
                            (r.clamp(0.0, 1.0) * 255.0) as u8,
                            (g.clamp(0.0, 1.0) * 255.0) as u8,
                            (b.clamp(0.0, 1.0) * 255.0) as u8,
                            (a.clamp(0.0, 1.0) * 255.0) as u8,
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

        image::RgbaImage::from_raw(self.width, self.height, rgba_data)
            .context("Failed to create image from buffer")
    }
}
