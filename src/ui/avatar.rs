use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use std::io::Read as IoRead;
use vulkano::{
    buffer::{Buffer, BufferCreateInfo, BufferUsage, Subbuffer},
    command_buffer::{AutoCommandBufferBuilder, CopyBufferToImageInfo, PrimaryAutoCommandBuffer},
    descriptor_set::{
        allocator::StandardDescriptorSetAllocator, DescriptorSet, WriteDescriptorSet,
    },
    device::DeviceOwned,
    format::Format,
    image::{
        sampler::{Filter, Sampler, SamplerCreateInfo},
        view::ImageView,
        Image, ImageCreateInfo, ImageType, ImageUsage,
    },
    memory::allocator::{AllocationCreateInfo, MemoryTypeFilter, StandardMemoryAllocator},
    pipeline::{
        graphics::{
            color_blend::{AttachmentBlend, ColorBlendAttachmentState, ColorBlendState},
            input_assembly::InputAssemblyState,
            multisample::MultisampleState,
            rasterization::RasterizationState,
            vertex_input::{Vertex, VertexDefinition},
            viewport::{Viewport, ViewportState},
            GraphicsPipelineCreateInfo,
        },
        layout::PipelineDescriptorSetLayoutCreateInfo,
        GraphicsPipeline, Pipeline, PipelineBindPoint, PipelineLayout,
        PipelineShaderStageCreateInfo,
    },
    render_pass::{RenderPass, Subpass},
};

use super::text::TextVertex;

// Avatar tile size in the atlas
const AVATAR_SIZE: u32 = 64;
// Atlas dimensions: 512x512 = 8x8 grid = 64 avatar slots
const ATLAS_WIDTH: u32 = 512;
const ATLAS_HEIGHT: u32 = 512;
const SLOTS_PER_ROW: u32 = ATLAS_WIDTH / AVATAR_SIZE;

// ============================================================================
// Avatar Cache (download management)
// ============================================================================

/// State of an avatar download
#[allow(dead_code)]
enum AvatarState {
    /// Not yet requested
    NotRequested,
    /// Download in progress
    Loading,
    /// Successfully downloaded and decoded
    Loaded { rgba: Vec<u8>, size: u32 },
    /// Download failed or user has no gravatar
    Failed,
}

/// Result from a background download thread
struct DownloadResult {
    email: String,
    data: Option<(Vec<u8>, u32)>, // (rgba pixels, size) or None on failure
}

/// Manages avatar downloads and caching
pub struct AvatarCache {
    states: HashMap<String, AvatarState>,
    sender: Sender<DownloadResult>,
    receiver: Receiver<DownloadResult>,
}

impl AvatarCache {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            states: HashMap::new(),
            sender,
            receiver,
        }
    }

    /// Request an avatar download for the given email if not already requested.
    pub fn request_avatar(&mut self, email: &str) {
        let key = email.trim().to_lowercase();
        if key.is_empty() || self.states.contains_key(&key) {
            return;
        }

        self.states.insert(key.clone(), AvatarState::Loading);
        let sender = self.sender.clone();

        std::thread::spawn(move || {
            let result = download_avatar(&key);
            let _ = sender.send(DownloadResult {
                email: key,
                data: result,
            });
        });
    }

    /// Poll for completed downloads and update states.
    /// Returns emails of newly loaded avatars.
    pub fn poll_downloads(&mut self) -> Vec<String> {
        let mut newly_loaded = Vec::new();
        while let Ok(result) = self.receiver.try_recv() {
            match result.data {
                Some((rgba, size)) => {
                    self.states.insert(
                        result.email.clone(),
                        AvatarState::Loaded { rgba, size },
                    );
                    newly_loaded.push(result.email);
                }
                None => {
                    self.states.insert(result.email, AvatarState::Failed);
                }
            }
        }
        newly_loaded
    }

    /// Check if an avatar is loaded for the given email.
    #[allow(dead_code)]
    pub fn is_loaded(&self, email: &str) -> bool {
        let key = email.trim().to_lowercase();
        matches!(self.states.get(&key), Some(AvatarState::Loaded { .. }))
    }

    /// Get loaded avatar data (rgba, size) for the given email.
    pub fn get_loaded(&self, email: &str) -> Option<(&[u8], u32)> {
        let key = email.trim().to_lowercase();
        match self.states.get(&key) {
            Some(AvatarState::Loaded { rgba, size }) => Some((rgba, *size)),
            _ => None,
        }
    }

    /// Check if avatar is in a terminal state (loaded or failed).
    #[allow(dead_code)]
    pub fn is_resolved(&self, email: &str) -> bool {
        let key = email.trim().to_lowercase();
        matches!(
            self.states.get(&key),
            Some(AvatarState::Loaded { .. } | AvatarState::Failed)
        )
    }
}

/// Download and decode a gravatar image. Returns RGBA pixels and size, or None on failure.
fn download_avatar(email: &str) -> Option<(Vec<u8>, u32)> {
    let cache_dir = dirs_cache_path();
    let hash = format!("{:x}", md5::compute(email));
    let cache_file = cache_dir.join(format!("{hash}.png"));

    // Check disk cache first
    if cache_file.exists() {
        if let Ok(img) = image::open(&cache_file) {
            let rgba_img = img.to_rgba8();
            let size = rgba_img.width();
            return Some((rgba_img.into_raw(), size));
        }
    }

    // Download from Gravatar
    let url = format!("https://www.gravatar.com/avatar/{hash}?s={AVATAR_SIZE}&d=404");
    let response = ureq::get(&url).call().ok()?;

    if response.status() != 200 {
        return None;
    }

    let mut bytes = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut bytes)
        .ok()?;

    // Decode image
    let img = image::load_from_memory(&bytes).ok()?;
    let rgba_img = img.resize_exact(
        AVATAR_SIZE,
        AVATAR_SIZE,
        image::imageops::FilterType::Lanczos3,
    ).to_rgba8();

    // Save to disk cache
    let _ = std::fs::create_dir_all(&cache_dir);
    let _ = rgba_img.save(&cache_file);

    Some((rgba_img.into_raw(), AVATAR_SIZE))
}

fn dirs_cache_path() -> std::path::PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            std::path::PathBuf::from(home).join(".cache")
        });
    base.join("whisper-git").join("avatars")
}

// ============================================================================
// Avatar Renderer (Vulkan pipeline + atlas)
// ============================================================================

/// Vertex shader for avatar rendering - identical to text renderer's shader
mod vs {
    vulkano_shaders::shader! {
        ty: "vertex",
        src: r"
            #version 450

            layout(location = 0) in vec2 position;
            layout(location = 1) in vec2 tex_coord;
            layout(location = 2) in vec4 color;

            layout(location = 0) out vec2 v_tex_coord;
            layout(location = 1) out vec4 v_color;

            layout(push_constant) uniform PushConstants {
                vec2 screen_size;
            } pc;

            void main() {
                vec2 ndc = (position / pc.screen_size) * 2.0 - 1.0;
                gl_Position = vec4(ndc.x, ndc.y, 0.0, 1.0);
                v_tex_coord = tex_coord;
                v_color = color;
            }
        ",
    }
}

/// Fragment shader for avatar rendering - samples RGBA texture directly
mod fs {
    vulkano_shaders::shader! {
        ty: "fragment",
        src: r"
            #version 450

            layout(location = 0) in vec2 v_tex_coord;
            layout(location = 1) in vec4 v_color;

            layout(location = 0) out vec4 f_color;

            layout(set = 0, binding = 0) uniform sampler2D avatar_atlas;

            void main() {
                vec4 texel = texture(avatar_atlas, v_tex_coord);
                f_color = texel * v_color;
            }
        ",
    }
}

/// Atlas slot info for a packed avatar
#[derive(Clone, Debug)]
pub struct AvatarSlot {
    /// Normalized texture coordinates [u0, v0, u1, v1]
    pub tex_coords: [f32; 4],
}

/// GPU-side avatar atlas renderer
pub struct AvatarRenderer {
    pipeline: Arc<GraphicsPipeline>,
    atlas_image: Arc<Image>,
    atlas_view: Arc<ImageView>,
    sampler: Arc<Sampler>,
    descriptor_set_allocator: Arc<StandardDescriptorSetAllocator>,
    memory_allocator: Arc<StandardMemoryAllocator>,
    /// Map from email to atlas slot
    slots: HashMap<String, AvatarSlot>,
    /// Next free slot index in the atlas grid
    next_slot: u32,
    /// Whether the atlas texture needs re-uploading
    atlas_dirty: bool,
    /// CPU-side atlas pixel data (RGBA)
    atlas_data: Vec<u8>,
}

impl AvatarRenderer {
    pub fn new(
        memory_allocator: Arc<StandardMemoryAllocator>,
        render_pass: Arc<RenderPass>,
    ) -> Result<Self> {
        let device = memory_allocator.device().clone();

        // Create atlas image (initialized to transparent black)
        let atlas_data = vec![0u8; (ATLAS_WIDTH * ATLAS_HEIGHT * 4) as usize];

        let atlas_image = Image::new(
            memory_allocator.clone(),
            ImageCreateInfo {
                image_type: ImageType::Dim2d,
                format: Format::R8G8B8A8_UNORM,
                extent: [ATLAS_WIDTH, ATLAS_HEIGHT, 1],
                usage: ImageUsage::TRANSFER_DST | ImageUsage::SAMPLED,
                ..Default::default()
            },
            AllocationCreateInfo::default(),
        )
        .context("Failed to create avatar atlas image")?;

        let atlas_view =
            ImageView::new_default(atlas_image.clone()).context("Failed to create avatar image view")?;

        let sampler = Sampler::new(
            device.clone(),
            SamplerCreateInfo {
                mag_filter: Filter::Linear,
                min_filter: Filter::Linear,
                ..Default::default()
            },
        )
        .context("Failed to create avatar sampler")?;

        // Create pipeline (same vertex format as TextRenderer)
        let vs = vs::load(device.clone()).context("Failed to load avatar vertex shader")?;
        let fs = fs::load(device.clone()).context("Failed to load avatar fragment shader")?;

        let vs_entry = vs.entry_point("main").unwrap();
        let fs_entry = fs.entry_point("main").unwrap();

        let vertex_input_state = TextVertex::per_vertex().definition(&vs_entry).unwrap();

        let stages = [
            PipelineShaderStageCreateInfo::new(vs_entry),
            PipelineShaderStageCreateInfo::new(fs_entry),
        ];

        let layout = PipelineLayout::new(
            device.clone(),
            PipelineDescriptorSetLayoutCreateInfo::from_stages(&stages)
                .into_pipeline_layout_create_info(device.clone())
                .context("Failed to create avatar pipeline layout info")?,
        )
        .context("Failed to create avatar pipeline layout")?;

        let subpass = Subpass::from(render_pass, 0).context("Failed to get subpass for avatar")?;

        let pipeline = GraphicsPipeline::new(
            device.clone(),
            None,
            GraphicsPipelineCreateInfo {
                stages: stages.into_iter().collect(),
                vertex_input_state: Some(vertex_input_state),
                input_assembly_state: Some(InputAssemblyState::default()),
                viewport_state: Some(ViewportState {
                    viewports: [Viewport::default()].into_iter().collect(),
                    ..Default::default()
                }),
                rasterization_state: Some(RasterizationState::default()),
                multisample_state: Some(MultisampleState::default()),
                color_blend_state: Some(ColorBlendState::with_attachment_states(
                    subpass.num_color_attachments(),
                    ColorBlendAttachmentState {
                        blend: Some(AttachmentBlend::alpha()),
                        ..Default::default()
                    },
                )),
                dynamic_state: [vulkano::pipeline::DynamicState::Viewport]
                    .into_iter()
                    .collect(),
                subpass: Some(subpass.into()),
                ..GraphicsPipelineCreateInfo::layout(layout)
            },
        )
        .context("Failed to create avatar graphics pipeline")?;

        let descriptor_set_allocator = Arc::new(StandardDescriptorSetAllocator::new(
            device.clone(),
            Default::default(),
        ));

        Ok(Self {
            pipeline,
            atlas_image,
            atlas_view,
            sampler,
            descriptor_set_allocator,
            memory_allocator,
            slots: HashMap::new(),
            next_slot: 0,
            atlas_dirty: false,
            atlas_data,
        })
    }

    /// Pack an avatar into the atlas. Returns true if successful, false if atlas is full.
    pub fn pack_avatar(&mut self, email: &str, rgba: &[u8], _size: u32) -> bool {
        let key = email.trim().to_lowercase();
        if self.slots.contains_key(&key) {
            return true; // Already packed
        }

        let max_slots = (ATLAS_WIDTH / AVATAR_SIZE) * (ATLAS_HEIGHT / AVATAR_SIZE);
        if self.next_slot >= max_slots {
            return false; // Atlas full
        }

        let slot_x = self.next_slot % SLOTS_PER_ROW;
        let slot_y = self.next_slot / SLOTS_PER_ROW;
        let px_x = slot_x * AVATAR_SIZE;
        let px_y = slot_y * AVATAR_SIZE;

        // Copy pixels into atlas data
        for row in 0..AVATAR_SIZE {
            let src_offset = (row * AVATAR_SIZE * 4) as usize;
            let dst_offset = ((px_y + row) * ATLAS_WIDTH * 4 + px_x * 4) as usize;
            let row_bytes = (AVATAR_SIZE * 4) as usize;
            if src_offset + row_bytes <= rgba.len()
                && dst_offset + row_bytes <= self.atlas_data.len()
            {
                self.atlas_data[dst_offset..dst_offset + row_bytes]
                    .copy_from_slice(&rgba[src_offset..src_offset + row_bytes]);
            }
        }

        // Record texture coordinates
        let u0 = px_x as f32 / ATLAS_WIDTH as f32;
        let v0 = px_y as f32 / ATLAS_HEIGHT as f32;
        let u1 = (px_x + AVATAR_SIZE) as f32 / ATLAS_WIDTH as f32;
        let v1 = (px_y + AVATAR_SIZE) as f32 / ATLAS_HEIGHT as f32;

        self.slots.insert(
            key,
            AvatarSlot {
                tex_coords: [u0, v0, u1, v1],
            },
        );

        self.next_slot += 1;
        self.atlas_dirty = true;
        true
    }

    /// Get the atlas texture coordinates for an avatar, if packed.
    pub fn get_tex_coords(&self, email: &str) -> Option<[f32; 4]> {
        let key = email.trim().to_lowercase();
        self.slots.get(&key).map(|s| s.tex_coords)
    }

    /// Upload dirty atlas data to the GPU. Call this inside a command buffer recording session.
    pub fn upload_atlas(
        &mut self,
        builder: &mut AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
    ) -> Result<()> {
        if !self.atlas_dirty {
            return Ok(());
        }

        let upload_buffer = Buffer::from_iter(
            self.memory_allocator.clone(),
            BufferCreateInfo {
                usage: BufferUsage::TRANSFER_SRC,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_HOST
                    | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                ..Default::default()
            },
            self.atlas_data.clone(),
        )
        .context("Failed to create avatar upload buffer")?;

        builder
            .copy_buffer_to_image(CopyBufferToImageInfo::buffer_image(
                upload_buffer,
                self.atlas_image.clone(),
            ))
            .context("Failed to copy avatar atlas to GPU")?;

        self.atlas_dirty = false;
        Ok(())
    }

    /// Create a vertex buffer from avatar vertices.
    pub fn create_vertex_buffer(
        &self,
        vertices: Vec<TextVertex>,
    ) -> Result<Subbuffer<[TextVertex]>> {
        Buffer::from_iter(
            self.memory_allocator.clone(),
            BufferCreateInfo {
                usage: BufferUsage::VERTEX_BUFFER,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                    | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                ..Default::default()
            },
            vertices,
        )
        .context("Failed to create avatar vertex buffer")
    }

    /// Draw avatar quads.
    pub fn draw(
        &self,
        builder: &mut AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
        vertex_buffer: Subbuffer<[TextVertex]>,
        viewport: Viewport,
    ) -> Result<()> {
        let layout = self.pipeline.layout().clone();
        let descriptor_set_layouts = layout.set_layouts();

        let descriptor_set = DescriptorSet::new(
            self.descriptor_set_allocator.clone(),
            descriptor_set_layouts[0].clone(),
            [WriteDescriptorSet::image_view_sampler(
                0,
                self.atlas_view.clone(),
                self.sampler.clone(),
            )],
            [],
        )
        .context("Failed to create avatar descriptor set")?;

        let vertex_count = vertex_buffer.len() as u32;

        builder
            .bind_pipeline_graphics(self.pipeline.clone())
            .context("Failed to bind avatar pipeline")?
            .bind_descriptor_sets(
                PipelineBindPoint::Graphics,
                layout.clone(),
                0,
                descriptor_set,
            )
            .context("Failed to bind avatar descriptor sets")?
            .push_constants(layout, 0, vs::PushConstants {
                screen_size: [viewport.extent[0], viewport.extent[1]],
            })
            .context("Failed to push avatar constants")?
            .set_viewport(0, [viewport].into_iter().collect())
            .context("Failed to set avatar viewport")?
            .bind_vertex_buffers(0, vertex_buffer)
            .context("Failed to bind avatar vertex buffers")?;

        unsafe {
            builder
                .draw(vertex_count, 1, 0, 0)
                .context("Failed to draw avatars")?;
        }

        Ok(())
    }
}

/// Create a textured quad (two triangles) for an avatar at the given position.
pub fn avatar_quad(
    x: f32,
    y: f32,
    size: f32,
    tex_coords: [f32; 4],
) -> [TextVertex; 6] {
    let [u0, v0, u1, v1] = tex_coords;
    let x1 = x + size;
    let y1 = y + size;
    let color = [1.0, 1.0, 1.0, 1.0]; // No tinting

    [
        TextVertex { position: [x, y], tex_coord: [u0, v0], color },
        TextVertex { position: [x1, y], tex_coord: [u1, v0], color },
        TextVertex { position: [x, y1], tex_coord: [u0, v1], color },
        TextVertex { position: [x1, y], tex_coord: [u1, v0], color },
        TextVertex { position: [x1, y1], tex_coord: [u1, v1], color },
        TextVertex { position: [x, y1], tex_coord: [u0, v1], color },
    ]
}
