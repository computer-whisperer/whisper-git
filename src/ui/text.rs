use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use std::collections::HashMap;
use std::sync::Arc;
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

/// Vertex for text rendering
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable, Vertex)]
pub struct TextVertex {
    #[format(R32G32_SFLOAT)]
    pub position: [f32; 2],
    #[format(R32G32_SFLOAT)]
    pub tex_coord: [f32; 2],
    #[format(R32G32B32A32_SFLOAT)]
    pub color: [f32; 4],
}

/// Cached glyph information
struct GlyphInfo {
    tex_x: f32,
    tex_y: f32,
    tex_w: f32,
    tex_h: f32,
    width: f32,
    height: f32,
    bearing_x: f32,
    bearing_y: f32,
    advance: f32,
}

/// Simple text renderer using a font atlas
///
/// The atlas is built at `atlas_scale` (typically the highest DPI monitor).
/// At runtime, `render_scale` can be changed (e.g. when moving between monitors).
/// All public metrics and glyph positions are scaled by `render_scale / atlas_scale`.
pub struct TextRenderer {
    pipeline: Arc<GraphicsPipeline>,
    font_texture: Arc<ImageView>,
    sampler: Arc<Sampler>,
    descriptor_set_allocator: Arc<StandardDescriptorSetAllocator>,
    memory_allocator: Arc<StandardMemoryAllocator>,
    glyphs: HashMap<char, GlyphInfo>,
    _atlas_width: f32,
    _atlas_height: f32,
    /// Line height in atlas pixels (physical pixels at atlas_scale)
    line_height: f32,
    /// Ascent in atlas pixels
    ascent: f32,
    /// Scale factor used when building the atlas
    atlas_scale: f32,
    /// Current display scale factor (updated on monitor changes)
    render_scale: f32,
}

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
                // Convert pixel coords to NDC
                vec2 ndc = (position / pc.screen_size) * 2.0 - 1.0;
                gl_Position = vec4(ndc.x, ndc.y, 0.0, 1.0);
                v_tex_coord = tex_coord;
                v_color = color;
            }
        ",
    }
}

mod fs {
    vulkano_shaders::shader! {
        ty: "fragment",
        src: r"
            #version 450

            layout(location = 0) in vec2 v_tex_coord;
            layout(location = 1) in vec4 v_color;

            layout(location = 0) out vec4 f_color;

            layout(set = 0, binding = 0) uniform sampler2D font_atlas;

            void main() {
                float alpha = texture(font_atlas, v_tex_coord).r;
                f_color = vec4(v_color.rgb, v_color.a * alpha);
            }
        ",
    }
}

impl TextRenderer {
    pub fn new(
        memory_allocator: Arc<StandardMemoryAllocator>,
        render_pass: Arc<RenderPass>,
        command_buffer_builder: &mut AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
        scale_factor: f64,
    ) -> Result<Self> {
        Self::new_with_font(
            memory_allocator,
            render_pass,
            command_buffer_builder,
            scale_factor,
            include_bytes!("/usr/share/fonts/TTF/Roboto-Regular.ttf"),
        )
    }

    /// Create a bold text renderer using Roboto-Bold
    pub fn new_bold(
        memory_allocator: Arc<StandardMemoryAllocator>,
        render_pass: Arc<RenderPass>,
        command_buffer_builder: &mut AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
        scale_factor: f64,
    ) -> Result<Self> {
        Self::new_with_font(
            memory_allocator,
            render_pass,
            command_buffer_builder,
            scale_factor,
            include_bytes!("/usr/share/fonts/TTF/Roboto-Bold.ttf"),
        )
    }

    fn new_with_font(
        memory_allocator: Arc<StandardMemoryAllocator>,
        render_pass: Arc<RenderPass>,
        command_buffer_builder: &mut AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
        scale_factor: f64,
        font_bytes: &[u8],
    ) -> Result<Self> {
        let device = memory_allocator.device().clone();

        let font = FontRef::try_from_slice(font_bytes).context("Failed to load font")?;

        // Build atlas at the given scale factor (should be the max across all monitors).
        // Runtime scaling is handled by render_scale / atlas_scale ratio.
        let base_font_size = 14.0_f64;
        let atlas_scale = scale_factor as f32;
        let scale = PxScale::from((base_font_size * scale_factor) as f32);
        let scaled_font = font.as_scaled(scale);
        let line_height = scaled_font.height();
        let ascent = scaled_font.ascent();

        // Characters to include in atlas
        let chars: Vec<char> = (32u8..127u8).map(|c| c as char).collect();

        // First pass: calculate atlas size
        let mut total_width = 0u32;
        let mut max_height = 0u32;
        let padding = 2u32;

        for &c in &chars {
            if let Some(glyph) = scaled_font.outline_glyph(scaled_font.scaled_glyph(c)) {
                let bounds = glyph.px_bounds();
                total_width += bounds.width() as u32 + padding;
                max_height = max_height.max(bounds.height() as u32 + padding);
            } else {
                // Space or non-renderable char
                total_width += (scaled_font.h_advance(scaled_font.glyph_id(c)) as u32).max(8) + padding;
            }
        }

        let atlas_width = total_width.next_power_of_two().max(256);
        let atlas_height = max_height.next_power_of_two().max(64);

        // Create atlas pixel data
        let mut atlas_data = vec![0u8; (atlas_width * atlas_height) as usize];
        let mut glyphs = HashMap::new();
        let mut x_offset = 0u32;

        for &c in &chars {
            let glyph_id = scaled_font.glyph_id(c);
            let advance = scaled_font.h_advance(glyph_id);

            if let Some(outlined) = scaled_font.outline_glyph(scaled_font.scaled_glyph(c)) {
                let bounds = outlined.px_bounds();
                let glyph_width = bounds.width() as u32;
                let glyph_height = bounds.height() as u32;

                // Rasterize glyph into atlas
                outlined.draw(|x, y, coverage| {
                    let px = x_offset + x;
                    let py = y;
                    if px < atlas_width && py < atlas_height {
                        atlas_data[(py * atlas_width + px) as usize] = (coverage * 255.0) as u8;
                    }
                });

                glyphs.insert(
                    c,
                    GlyphInfo {
                        tex_x: x_offset as f32 / atlas_width as f32,
                        tex_y: 0.0,
                        tex_w: glyph_width as f32 / atlas_width as f32,
                        tex_h: glyph_height as f32 / atlas_height as f32,
                        width: glyph_width as f32,
                        height: glyph_height as f32,
                        bearing_x: bounds.min.x,
                        bearing_y: bounds.min.y,
                        advance,
                    },
                );

                x_offset += glyph_width + padding;
            } else {
                // Space or non-renderable - just store advance
                glyphs.insert(
                    c,
                    GlyphInfo {
                        tex_x: 0.0,
                        tex_y: 0.0,
                        tex_w: 0.0,
                        tex_h: 0.0,
                        width: 0.0,
                        height: 0.0,
                        bearing_x: 0.0,
                        bearing_y: 0.0,
                        advance,
                    },
                );
            }
        }

        // Upload atlas to GPU
        let upload_buffer = Buffer::from_iter(
            memory_allocator.clone(),
            BufferCreateInfo {
                usage: BufferUsage::TRANSFER_SRC,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_HOST
                    | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                ..Default::default()
            },
            atlas_data,
        )
        .context("Failed to create upload buffer")?;

        let font_image = Image::new(
            memory_allocator.clone(),
            ImageCreateInfo {
                image_type: ImageType::Dim2d,
                format: Format::R8_UNORM,
                extent: [atlas_width, atlas_height, 1],
                usage: ImageUsage::TRANSFER_DST | ImageUsage::SAMPLED,
                ..Default::default()
            },
            AllocationCreateInfo::default(),
        )
        .context("Failed to create font texture")?;

        command_buffer_builder
            .copy_buffer_to_image(CopyBufferToImageInfo::buffer_image(
                upload_buffer,
                font_image.clone(),
            ))
            .context("Failed to copy buffer to image")?;

        let font_texture =
            ImageView::new_default(font_image).context("Failed to create image view")?;

        let sampler = Sampler::new(
            device.clone(),
            SamplerCreateInfo {
                mag_filter: Filter::Linear,
                min_filter: Filter::Linear,
                ..Default::default()
            },
        )
        .context("Failed to create sampler")?;

        // Create pipeline
        let vs = vs::load(device.clone()).context("Failed to load vertex shader")?;
        let fs = fs::load(device.clone()).context("Failed to load fragment shader")?;

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
                .context("Failed to create pipeline layout info")?,
        )
        .context("Failed to create pipeline layout")?;

        let subpass = Subpass::from(render_pass, 0).context("Failed to get subpass")?;

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
                dynamic_state: [vulkano::pipeline::DynamicState::Viewport].into_iter().collect(),
                subpass: Some(subpass.into()),
                ..GraphicsPipelineCreateInfo::layout(layout)
            },
        )
        .context("Failed to create graphics pipeline")?;

        let descriptor_set_allocator = Arc::new(StandardDescriptorSetAllocator::new(
            device.clone(),
            Default::default(),
        ));

        Ok(Self {
            pipeline,
            font_texture,
            sampler,
            descriptor_set_allocator,
            memory_allocator,
            glyphs,
            _atlas_width: atlas_width as f32,
            _atlas_height: atlas_height as f32,
            line_height,
            ascent,
            atlas_scale,
            render_scale: atlas_scale, // Initially matches atlas; updated on monitor change
        })
    }

    /// Ratio of current display scale to atlas scale.
    /// Multiply atlas-pixel values by this to get current physical pixels.
    fn scale_ratio(&self) -> f32 {
        self.render_scale / self.atlas_scale
    }

    /// Update the render scale (call when moving between monitors).
    /// The atlas stays the same; metrics and glyph positions adjust via the ratio.
    pub fn set_render_scale(&mut self, scale: f64) {
        self.render_scale = scale as f32;
    }

    /// Create vertices for a text string
    ///
    /// The y parameter is the TOP of the text line. Text is positioned using
    /// the font's ascent to compute the baseline, ensuring proper alignment
    /// of characters with different heights.
    pub fn layout_text(
        &self,
        text: &str,
        x: f32,
        y: f32,
        color: [f32; 4],
    ) -> Vec<TextVertex> {
        let ratio = self.scale_ratio();
        let mut vertices = Vec::new();
        let mut cursor_x = x;
        // Compute baseline from top of line: baseline = top + ascent (scaled)
        let baseline_y = y + self.ascent * ratio;

        for c in text.chars() {
            if let Some(glyph) = self.glyphs.get(&c) {
                if glyph.width > 0.0 {
                    // Quad positions: scaled from atlas pixels to current physical pixels
                    let x0 = cursor_x + glyph.bearing_x * ratio;
                    let y0 = baseline_y + glyph.bearing_y * ratio;
                    let x1 = x0 + glyph.width * ratio;
                    let y1 = y0 + glyph.height * ratio;

                    // Texture coordinates (unchanged - reference same atlas region)
                    let u0 = glyph.tex_x;
                    let v0 = glyph.tex_y;
                    let u1 = glyph.tex_x + glyph.tex_w;
                    let v1 = glyph.tex_y + glyph.tex_h;

                    // Two triangles for quad
                    vertices.push(TextVertex { position: [x0, y0], tex_coord: [u0, v0], color });
                    vertices.push(TextVertex { position: [x1, y0], tex_coord: [u1, v0], color });
                    vertices.push(TextVertex { position: [x0, y1], tex_coord: [u0, v1], color });

                    vertices.push(TextVertex { position: [x1, y0], tex_coord: [u1, v0], color });
                    vertices.push(TextVertex { position: [x1, y1], tex_coord: [u1, v1], color });
                    vertices.push(TextVertex { position: [x0, y1], tex_coord: [u0, v1], color });
                }

                cursor_x += glyph.advance * ratio;
            }
        }

        vertices
    }

    /// Get line height for text layout (scaled to current display)
    pub fn line_height(&self) -> f32 {
        self.line_height * self.scale_ratio()
    }

    /// Get line height for small (secondary) text - 85% of normal
    pub fn line_height_small(&self) -> f32 {
        self.line_height() * 0.85
    }

    /// Layout text at a smaller (secondary) size - 85% of normal.
    /// Reuses the existing atlas, just renders smaller quads.
    pub fn layout_text_small(&self, text: &str, x: f32, y: f32, color: [f32; 4]) -> Vec<TextVertex> {
        self.layout_text_scaled(text, x, y, color, 0.85)
    }

    /// Layout text at a custom scale factor relative to normal size.
    /// Reuses the existing atlas, rendering smaller or larger quads.
    pub fn layout_text_scaled(&self, text: &str, x: f32, y: f32, color: [f32; 4], text_scale: f32) -> Vec<TextVertex> {
        let ratio = self.scale_ratio() * text_scale;
        let mut vertices = Vec::new();
        let mut cursor_x = x;
        let baseline_y = y + self.ascent * ratio;

        for c in text.chars() {
            if let Some(glyph) = self.glyphs.get(&c) {
                if glyph.width > 0.0 {
                    let x0 = cursor_x + glyph.bearing_x * ratio;
                    let y0 = baseline_y + glyph.bearing_y * ratio;
                    let x1 = x0 + glyph.width * ratio;
                    let y1 = y0 + glyph.height * ratio;

                    let u0 = glyph.tex_x;
                    let v0 = glyph.tex_y;
                    let u1 = glyph.tex_x + glyph.tex_w;
                    let v1 = glyph.tex_y + glyph.tex_h;

                    vertices.push(TextVertex { position: [x0, y0], tex_coord: [u0, v0], color });
                    vertices.push(TextVertex { position: [x1, y0], tex_coord: [u1, v0], color });
                    vertices.push(TextVertex { position: [x0, y1], tex_coord: [u0, v1], color });

                    vertices.push(TextVertex { position: [x1, y0], tex_coord: [u1, v0], color });
                    vertices.push(TextVertex { position: [x1, y1], tex_coord: [u1, v1], color });
                    vertices.push(TextVertex { position: [x0, y1], tex_coord: [u0, v1], color });
                }

                cursor_x += glyph.advance * ratio;
            }
        }

        vertices
    }

    /// Measure the width of a text string at a custom scale factor
    pub fn measure_text_scaled(&self, text: &str, text_scale: f32) -> f32 {
        let ratio = self.scale_ratio() * text_scale;
        text.chars()
            .filter_map(|c| self.glyphs.get(&c))
            .map(|g| g.advance * ratio)
            .sum()
    }

    /// Get character width (advance) for a monospace font (scaled to current display)
    pub fn char_width(&self) -> f32 {
        self.glyphs
            .get(&'M')
            .map(|g| g.advance * self.scale_ratio())
            .unwrap_or(8.0 * self.render_scale)
    }

    /// Measure the width of a text string in pixels (scaled to current display)
    pub fn measure_text(&self, text: &str) -> f32 {
        let ratio = self.scale_ratio();
        text.chars()
            .filter_map(|c| self.glyphs.get(&c))
            .map(|g| g.advance * ratio)
            .sum()
    }

    /// Create a vertex buffer from vertices
    pub fn create_vertex_buffer(&self, vertices: Vec<TextVertex>) -> Result<Subbuffer<[TextVertex]>> {
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
        .context("Failed to create vertex buffer")
    }

    /// Draw text
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
                self.font_texture.clone(),
                self.sampler.clone(),
            )],
            [],
        )
        .context("Failed to create descriptor set")?;

        let vertex_count = vertex_buffer.len() as u32;

        builder
            .bind_pipeline_graphics(self.pipeline.clone())
            .context("Failed to bind pipeline")?
            .bind_descriptor_sets(
                PipelineBindPoint::Graphics,
                layout.clone(),
                0,
                descriptor_set,
            )
            .context("Failed to bind descriptor sets")?
            .push_constants(layout, 0, vs::PushConstants {
                screen_size: [viewport.extent[0], viewport.extent[1]],
            })
            .context("Failed to push constants")?
            .set_viewport(0, [viewport].into_iter().collect())
            .context("Failed to set viewport")?
            .bind_vertex_buffers(0, vertex_buffer)
            .context("Failed to bind vertex buffers")?;

        // SAFETY: We've bound all required state (pipeline, descriptor sets, vertex buffers)
        // and the vertex count matches the buffer length
        unsafe {
            builder.draw(vertex_count, 1, 0, 0).context("Failed to draw")?;
        }

        Ok(())
    }
}
