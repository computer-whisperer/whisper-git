//! Generic icon rendering via a GPU texture atlas.
//!
//! Icons are registered by name from RGBA pixel data and packed into a shared atlas texture.
//! Rendered as textured quads using the same vertex format as the text renderer.
//! The fragment shader applies a tint color, treating the icon alpha as a mask.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use vulkano::{
    buffer::{Buffer, BufferCreateInfo, BufferUsage, Subbuffer},
    command_buffer::{AutoCommandBufferBuilder, CopyBufferToImageInfo, PrimaryAutoCommandBuffer},
    descriptor_set::{
        DescriptorSet, WriteDescriptorSet, allocator::StandardDescriptorSetAllocator,
    },
    device::DeviceOwned,
    format::Format,
    image::{
        Image, ImageCreateInfo, ImageType, ImageUsage,
        sampler::{Filter, Sampler, SamplerCreateInfo},
        view::ImageView,
    },
    memory::allocator::{AllocationCreateInfo, MemoryTypeFilter, StandardMemoryAllocator},
    pipeline::{
        GraphicsPipeline, Pipeline, PipelineBindPoint, PipelineLayout,
        PipelineShaderStageCreateInfo,
        graphics::{
            GraphicsPipelineCreateInfo,
            color_blend::{AttachmentBlend, ColorBlendAttachmentState, ColorBlendState},
            input_assembly::InputAssemblyState,
            multisample::MultisampleState,
            rasterization::RasterizationState,
            vertex_input::{Vertex, VertexDefinition},
            viewport::{Viewport, ViewportState},
        },
        layout::PipelineDescriptorSetLayoutCreateInfo,
    },
    render_pass::{RenderPass, Subpass},
};

use super::text::TextVertex;

// Atlas dimensions — 256x256 is plenty for icons
const ATLAS_WIDTH: u32 = 256;
const ATLAS_HEIGHT: u32 = 256;

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

/// Fragment shader: samples icon atlas alpha and applies tint color.
mod fs {
    vulkano_shaders::shader! {
        ty: "fragment",
        src: r"
            #version 450

            layout(location = 0) in vec2 v_tex_coord;
            layout(location = 1) in vec4 v_color;

            layout(location = 0) out vec4 f_color;

            layout(set = 0, binding = 0) uniform sampler2D icon_atlas;

            vec3 srgb_to_linear(vec3 c) {
                bvec3 cutoff = lessThanEqual(c, vec3(0.04045));
                vec3 low = c / 12.92;
                vec3 high = pow((c + 0.055) / 1.055, vec3(2.4));
                return mix(high, low, cutoff);
            }

            void main() {
                vec4 texel = texture(icon_atlas, v_tex_coord);
                // Use texel alpha as mask, tint with v_color
                vec3 tint = srgb_to_linear(v_color.rgb);
                f_color = vec4(tint, texel.a * v_color.a);
            }
        ",
    }
}

/// Packed icon slot in the atlas
struct IconSlot {
    /// Normalized texture coordinates [u0, v0, u1, v1]
    tex_coords: [f32; 4],
}

/// GPU-side icon atlas renderer.
pub struct IconRenderer {
    pipeline: Arc<GraphicsPipeline>,
    atlas_image: Arc<Image>,
    atlas_view: Arc<ImageView>,
    sampler: Arc<Sampler>,
    descriptor_set_allocator: Arc<StandardDescriptorSetAllocator>,
    memory_allocator: Arc<StandardMemoryAllocator>,
    slots: HashMap<String, IconSlot>,
    /// Simple row-based packer state
    pack_x: u32,
    pack_y: u32,
    pack_row_height: u32,
    atlas_dirty: bool,
    atlas_data: Vec<u8>,
}

impl IconRenderer {
    pub fn new(
        memory_allocator: Arc<StandardMemoryAllocator>,
        render_pass: Arc<RenderPass>,
    ) -> Result<Self> {
        let device = memory_allocator.device().clone();

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
        .context("Failed to create icon atlas image")?;

        let atlas_view = ImageView::new_default(atlas_image.clone())
            .context("Failed to create icon image view")?;

        let sampler = Sampler::new(
            device.clone(),
            SamplerCreateInfo {
                mag_filter: Filter::Linear,
                min_filter: Filter::Linear,
                ..Default::default()
            },
        )
        .context("Failed to create icon sampler")?;

        let vs = vs::load(device.clone()).context("Failed to load icon vertex shader")?;
        let fs = fs::load(device.clone()).context("Failed to load icon fragment shader")?;

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
                .context("Failed to create icon pipeline layout info")?,
        )
        .context("Failed to create icon pipeline layout")?;

        let subpass = Subpass::from(render_pass, 0).context("Failed to get subpass for icon")?;

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
                multisample_state: Some(MultisampleState {
                    rasterization_samples: vulkano::image::SampleCount::Sample4,
                    ..Default::default()
                }),
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
        .context("Failed to create icon graphics pipeline")?;

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
            pack_x: 0,
            pack_y: 0,
            pack_row_height: 0,
            atlas_dirty: false,
            atlas_data,
        })
    }

    /// Register an icon from RGBA pixel data. Returns true on success.
    pub fn register(&mut self, name: &str, rgba: &[u8], width: u32, height: u32) -> bool {
        if self.slots.contains_key(name) {
            return true;
        }

        // Simple row-based packing: try current row, else start new row
        if self.pack_x + width > ATLAS_WIDTH {
            self.pack_x = 0;
            self.pack_y += self.pack_row_height;
            self.pack_row_height = 0;
        }
        if self.pack_y + height > ATLAS_HEIGHT {
            return false; // Atlas full
        }

        let px_x = self.pack_x;
        let px_y = self.pack_y;

        // Copy pixels into atlas
        for row in 0..height {
            let src_offset = (row * width * 4) as usize;
            let dst_offset = ((px_y + row) * ATLAS_WIDTH * 4 + px_x * 4) as usize;
            let row_bytes = (width * 4) as usize;
            if src_offset + row_bytes <= rgba.len()
                && dst_offset + row_bytes <= self.atlas_data.len()
            {
                self.atlas_data[dst_offset..dst_offset + row_bytes]
                    .copy_from_slice(&rgba[src_offset..src_offset + row_bytes]);
            }
        }

        let u0 = px_x as f32 / ATLAS_WIDTH as f32;
        let v0 = px_y as f32 / ATLAS_HEIGHT as f32;
        let u1 = (px_x + width) as f32 / ATLAS_WIDTH as f32;
        let v1 = (px_y + height) as f32 / ATLAS_HEIGHT as f32;

        self.slots.insert(
            name.to_string(),
            IconSlot {
                tex_coords: [u0, v0, u1, v1],
            },
        );

        self.pack_x += width;
        self.pack_row_height = self.pack_row_height.max(height);
        self.atlas_dirty = true;
        true
    }

    /// Register an icon from an embedded PNG image.
    pub fn register_png(&mut self, name: &str, png_bytes: &[u8]) -> bool {
        let img = match image::load_from_memory(png_bytes) {
            Ok(img) => img.to_rgba8(),
            Err(_) => return false,
        };
        let (w, h) = (img.width(), img.height());
        self.register(name, img.as_raw(), w, h)
    }

    /// Get atlas texture coordinates for a named icon.
    pub fn get_tex_coords(&self, name: &str) -> Option<[f32; 4]> {
        self.slots.get(name).map(|s| s.tex_coords)
    }

    pub fn needs_upload(&self) -> bool {
        self.atlas_dirty
    }

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
        .context("Failed to create icon upload buffer")?;

        builder
            .copy_buffer_to_image(CopyBufferToImageInfo::buffer_image(
                upload_buffer,
                self.atlas_image.clone(),
            ))
            .context("Failed to copy icon atlas to GPU")?;

        self.atlas_dirty = false;
        Ok(())
    }

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
        .context("Failed to create icon vertex buffer")
    }

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
        .context("Failed to create icon descriptor set")?;

        let vertex_count = vertex_buffer.len() as u32;

        builder
            .bind_pipeline_graphics(self.pipeline.clone())
            .context("Failed to bind icon pipeline")?
            .bind_descriptor_sets(
                PipelineBindPoint::Graphics,
                layout.clone(),
                0,
                descriptor_set,
            )
            .context("Failed to bind icon descriptor sets")?
            .push_constants(
                layout,
                0,
                vs::PushConstants {
                    screen_size: [viewport.extent[0], viewport.extent[1]],
                },
            )
            .context("Failed to push icon constants")?
            .set_viewport(0, [viewport].into_iter().collect())
            .context("Failed to set icon viewport")?
            .bind_vertex_buffers(0, vertex_buffer)
            .context("Failed to bind icon vertex buffers")?;

        unsafe {
            builder
                .draw(vertex_count, 1, 0, 0)
                .context("Failed to draw icons")?;
        }

        Ok(())
    }
}

/// Create a textured quad for an icon at the given position with a tint color.
/// Unlike avatar_quad, this renders the full rectangle (no circle clipping).
pub fn icon_quad(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    tex_coords: [f32; 4],
    tint: [f32; 4],
) -> [TextVertex; 6] {
    let [u0, v0, u1, v1] = tex_coords;
    let x1 = x + w;
    let y1 = y + h;

    [
        TextVertex {
            position: [x, y],
            tex_coord: [u0, v0],
            color: tint,
        },
        TextVertex {
            position: [x1, y],
            tex_coord: [u1, v0],
            color: tint,
        },
        TextVertex {
            position: [x, y1],
            tex_coord: [u0, v1],
            color: tint,
        },
        TextVertex {
            position: [x1, y],
            tex_coord: [u1, v0],
            color: tint,
        },
        TextVertex {
            position: [x1, y1],
            tex_coord: [u1, v1],
            color: tint,
        },
        TextVertex {
            position: [x, y1],
            tex_coord: [u0, v1],
            color: tint,
        },
    ]
}

/// Built-in icon names
pub const ICON_GITHUB: &str = "github";

/// Register all built-in icons.
pub fn register_builtin_icons(renderer: &mut IconRenderer) {
    let github_png = include_bytes!("../../assets/icons/github.png");
    renderer.register_png(ICON_GITHUB, github_png);
}
