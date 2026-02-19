//! SDF text rendering via fontdue and coverage-aware contour distance.
//!
//! Rasterizes glyphs with fontdue, extracts a 0.5-coverage contour, and measures
//! signed distance to that contour to generate an SDF atlas (R8_UNORM),
//! and renders via vertex-based layout with smoothstep + fwidth shader for crisp text at any scale.

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use fontdue::Font;
use std::collections::{BTreeSet, HashMap};
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
        sampler::{Filter, Sampler, SamplerAddressMode, SamplerCreateInfo, SamplerMipmapMode},
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

/// SDF text renderer using a font atlas
///
/// The atlas is built at `atlas_scale` (typically the highest DPI monitor).
/// At runtime, `render_scale` can be changed (e.g. when moving between monitors).
/// All public metrics and glyph positions are scaled by `render_scale / atlas_scale`.
///
/// Uses fontdue for coverage rasterization, then computes a signed distance field
/// from the 0.5 coverage contour (preserving grayscale edge information).
/// The fragment shader uses smoothstep for crisp, resolution-independent
/// antialiased edges.
pub struct TextRenderer {
    pipeline: Arc<GraphicsPipeline>,
    font_texture: Arc<ImageView>,
    sampler: Arc<Sampler>,
    descriptor_set_allocator: Arc<StandardDescriptorSetAllocator>,
    memory_allocator: Arc<StandardMemoryAllocator>,
    glyphs: HashMap<char, GlyphInfo>,
    kerning: HashMap<(char, char), f32>,
    fallback_char: char,
    _atlas_width: f32,
    _atlas_height: f32,
    /// Line height in atlas pixels (physical pixels at atlas_scale)
    line_height: f32,
    /// Ascent in atlas pixels
    ascent: f32,
    /// Scale factor used when building the atlas
    atlas_scale: f32,
    /// Current display scale factor from winit (updated on monitor changes).
    /// This is in window scale units (not multiplied by atlas oversample).
    render_scale: f32,
    /// SDF threshold (0..1). 0.5 is the contour center for our atlas encoding.
    sdf_edge_center: f32,
    /// Multiplier applied to fwidth(dist) for edge antialiasing softness.
    sdf_aa_scale: f32,
}

/// Extra glyphs that are used in UI labels outside printable ASCII.
const EXTRA_GLYPHS: &[char] = &[
    '\u{2014}', // —
    '\u{2191}', // ↑
    '\u{2192}', // →
    '\u{2193}', // ↓
    '\u{2261}', // ≡
    '\u{25B2}', // ▲
    '\u{25BC}', // ▼
    '\u{25C6}', // ◆
    '\u{25C8}', // ◈
    '\u{25CB}', // ○
    '\u{25CF}', // ●
    '\u{2601}', // ☁
    '\u{2691}', // ⚑
    '\u{26A0}', // ⚠
    '\u{2713}', // ✓
    '\u{2715}', // ✕
    '\u{FFFD}', // replacement glyph
];

/// Additional Unicode blocks to include in the atlas.
/// Kept intentionally limited to avoid runaway atlas width with our single-row packer.
const EXTRA_GLYPH_BLOCKS: &[(u32, u32)] = &[
    (0x00A0, 0x00FF), // Latin-1 Supplement
    (0x0100, 0x017F), // Latin Extended-A
    (0x2000, 0x206F), // General Punctuation
];

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
                vec2 sdf_params;
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
            layout(push_constant) uniform PushConstants {
                vec2 screen_size;
                vec2 sdf_params;
            } pc;

            void main() {
                float d = texture(font_atlas, v_tex_coord).r;
                float aa_width = max(fwidth(d) * pc.sdf_params.y, 1.0 / 255.0);
                float alpha = smoothstep(pc.sdf_params.x - aa_width, pc.sdf_params.x + aa_width, d);
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
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        enum GlyphFont {
            Primary,
            Fallback,
        }

        #[inline]
        fn pick_glyph_font(c: char, primary: &Font, fallback: &Font) -> Option<GlyphFont> {
            if primary.has_glyph(c) {
                Some(GlyphFont::Primary)
            } else if fallback.has_glyph(c) {
                Some(GlyphFont::Fallback)
            } else {
                None
            }
        }

        let device = memory_allocator.device().clone();

        let font = Font::from_bytes(font_bytes, fontdue::FontSettings::default())
            .map_err(|e| anyhow::anyhow!(e))?;
        // Fallback with broader Unicode coverage than Roboto.
        let fallback_bytes: &[u8] = include_bytes!("/usr/share/fonts/TTF/DejaVuSans.ttf");
        let fallback_font = Font::from_bytes(fallback_bytes, fontdue::FontSettings::default())
            .map_err(|e| anyhow::anyhow!(e))?;

        // Build the atlas at 2x display resolution for high-quality SDF.
        // We rasterize coverage with fontdue at 2x and compute signed distance
        // to the 0.5-coverage contour. This preserves anti-aliased coverage
        // information (instead of hard-thresholding), reducing rough edges at
        // 1x displays while still supporting high-quality SDF scaling.
        // The smoothstep shader handles minification via fwidth-based AA.
        let base_font_size = 14.0_f64;
        let atlas_oversample = 2.0_f32;
        let display_font_size = (base_font_size * scale_factor) as f32;
        let font_size_px = display_font_size * atlas_oversample;
        let atlas_scale = scale_factor as f32 * atlas_oversample;

        // SDF spread: how many atlas pixels of distance gradient around the glyph edge.
        // Each glyph gets `spread` pixels of padding on each side.
        let sdf_spread: u32 = 4;

        // Get line metrics from fontdue
        let line_metrics = font
            .horizontal_line_metrics(font_size_px)
            .context("Failed to get line metrics")?;
        let line_height = line_metrics.new_line_size;
        let ascent = line_metrics.ascent;

        // Characters to include in atlas: printable ASCII + UI symbols + selected Unicode blocks.
        // BTreeSet keeps this deterministic while deduplicating.
        let mut char_set: BTreeSet<char> = (32u8..127u8).map(|c| c as char).collect();
        char_set.extend(EXTRA_GLYPHS.iter().copied());
        for &(start, end) in EXTRA_GLYPH_BLOCKS {
            for cp in start..=end {
                if let Some(c) = char::from_u32(cp) {
                    char_set.insert(c);
                }
            }
        }
        // Keep only codepoints backed by one of our fonts.
        let chars: Vec<char> = char_set
            .into_iter()
            .filter(|&c| pick_glyph_font(c, &font, &fallback_font).is_some())
            .collect();

        let mut glyph_fonts = HashMap::<char, GlyphFont>::with_capacity(chars.len());
        for &c in &chars {
            if let Some(slot) = pick_glyph_font(c, &font, &fallback_font) {
                glyph_fonts.insert(c, slot);
            }
        }

        // First pass: calculate atlas size (including SDF padding)
        let mut total_width = 0u32;
        let mut max_height = 0u32;
        let padding = 2u32;

        for &c in &chars {
            let metrics = match glyph_fonts.get(&c).copied() {
                Some(GlyphFont::Primary) => font.metrics(c, font_size_px),
                Some(GlyphFont::Fallback) => fallback_font.metrics(c, font_size_px),
                None => continue,
            };
            if metrics.width > 0 && metrics.height > 0 {
                let sdf_w = metrics.width as u32 + 2 * sdf_spread;
                let sdf_h = metrics.height as u32 + 2 * sdf_spread;
                total_width += sdf_w + padding;
                max_height = max_height.max(sdf_h + padding);
            } else {
                total_width += (metrics.advance_width as u32).max(8) + padding;
            }
        }

        let atlas_width = total_width.next_power_of_two().max(256);
        let atlas_height = max_height.next_power_of_two().max(64);

        // Create atlas pixel data (SDF: 128 = edge, >128 = inside, <128 = outside)
        let mut atlas_data = vec![0u8; (atlas_width * atlas_height) as usize];
        let mut glyphs = HashMap::new();
        let mut x_offset = 0u32;

        for &c in &chars {
            let (metrics, bitmap) = match glyph_fonts.get(&c).copied() {
                Some(GlyphFont::Primary) => font.rasterize(c, font_size_px),
                Some(GlyphFont::Fallback) => fallback_font.rasterize(c, font_size_px),
                None => continue,
            };

            if metrics.width > 0 && metrics.height > 0 {
                let cov_w = metrics.width;
                let cov_h = metrics.height;

                // Compute SDF from coverage bitmap via EDT
                let (sdf_bitmap, sdf_w, sdf_h) =
                    coverage_to_sdf(&bitmap, cov_w, cov_h, sdf_spread as usize);

                // Copy SDF into atlas
                for row in 0..sdf_h {
                    for col in 0..sdf_w {
                        let px = x_offset + col as u32;
                        let py = row as u32;
                        if px < atlas_width && py < atlas_height {
                            atlas_data[(py * atlas_width + px) as usize] =
                                sdf_bitmap[row * sdf_w + col];
                        }
                    }
                }

                // bearing_y: convert fontdue Y-up to screen Y-down, then expand by spread
                let bearing_y = -((metrics.ymin as f32) + (cov_h as f32)) - sdf_spread as f32;

                glyphs.insert(
                    c,
                    GlyphInfo {
                        tex_x: x_offset as f32 / atlas_width as f32,
                        tex_y: 0.0,
                        tex_w: sdf_w as f32 / atlas_width as f32,
                        tex_h: sdf_h as f32 / atlas_height as f32,
                        width: sdf_w as f32,
                        height: sdf_h as f32,
                        bearing_x: metrics.xmin as f32 - sdf_spread as f32,
                        bearing_y,
                        advance: metrics.advance_width,
                    },
                );

                x_offset += sdf_w as u32 + padding;
            } else {
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
                        advance: metrics.advance_width,
                    },
                );
            }
        }

        // Precompute kerning pairs for all atlas glyphs.
        let mut kerning = HashMap::new();
        for &left in &chars {
            for &right in &chars {
                let left_slot = glyph_fonts.get(&left).copied();
                let right_slot = glyph_fonts.get(&right).copied();
                let kern = match (left_slot, right_slot) {
                    (Some(GlyphFont::Primary), Some(GlyphFont::Primary)) => {
                        font.horizontal_kern(left, right, font_size_px)
                    }
                    (Some(GlyphFont::Fallback), Some(GlyphFont::Fallback)) => {
                        fallback_font.horizontal_kern(left, right, font_size_px)
                    }
                    _ => None,
                };
                if let Some(kern) = kern && kern != 0.0 {
                    kerning.insert((left, right), kern);
                }
            }
        }

        let fallback_char = ['\u{FFFD}', '?', ' ']
            .into_iter()
            .find(|c| glyphs.contains_key(c))
            .or_else(|| chars.first().copied())
            .unwrap_or('?');

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
                mipmap_mode: SamplerMipmapMode::Nearest,
                address_mode: [SamplerAddressMode::ClampToEdge; 3],
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
            kerning,
            fallback_char,
            _atlas_width: atlas_width as f32,
            _atlas_height: atlas_height as f32,
            line_height,
            ascent,
            atlas_scale,
            render_scale: scale_factor as f32,
            sdf_edge_center: 0.5,
            sdf_aa_scale: 0.75,
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

    /// Tune SDF edge rendering parameters.
    /// `edge_center` should usually remain near `0.5`; `aa_scale` typically in `[0.5, 1.5]`.
    #[allow(dead_code)]
    pub fn set_sdf_tuning(&mut self, edge_center: f32, aa_scale: f32) {
        self.sdf_edge_center = edge_center.clamp(0.0, 1.0);
        self.sdf_aa_scale = aa_scale.max(0.05);
    }

    fn glyph_for(&self, c: char) -> Option<(char, &GlyphInfo)> {
        if let Some(glyph) = self.glyphs.get(&c) {
            return Some((c, glyph));
        }
        self.glyphs
            .get(&self.fallback_char)
            .map(|glyph| (self.fallback_char, glyph))
    }

    #[inline]
    fn snap_to_pixel(v: f32) -> f32 {
        v.round()
    }

    fn measure_text_internal(&self, text: &str, ratio: f32) -> f32 {
        let mut width = 0.0;
        let mut prev_char: Option<char> = None;

        for c in text.chars() {
            let Some((resolved_char, glyph)) = self.glyph_for(c) else {
                continue;
            };

            if let Some(prev) = prev_char {
                width += self
                    .kerning
                    .get(&(prev, resolved_char))
                    .copied()
                    .unwrap_or(0.0)
                    * ratio;
            }

            width += glyph.advance * ratio;
            prev_char = Some(resolved_char);
        }

        width
    }

    fn layout_text_internal(
        &self,
        text: &str,
        x: f32,
        y: f32,
        color: [f32; 4],
        text_scale: f32,
    ) -> Vec<TextVertex> {
        let ratio = self.scale_ratio() * text_scale;
        let mut vertices = Vec::new();
        let mut cursor_x = Self::snap_to_pixel(x);
        // Compute baseline from top of line: baseline = top + ascent (scaled)
        let baseline_y = Self::snap_to_pixel(y + self.ascent * ratio);
        let mut prev_char: Option<char> = None;

        for c in text.chars() {
            let Some((resolved_char, glyph)) = self.glyph_for(c) else {
                continue;
            };

            if let Some(prev) = prev_char {
                cursor_x += self
                    .kerning
                    .get(&(prev, resolved_char))
                    .copied()
                    .unwrap_or(0.0)
                    * ratio;
            }

            if glyph.width > 0.0 {
                // Snap only the glyph origin to avoid spacing distortion at fractional DPI scales.
                let x0 = Self::snap_to_pixel(cursor_x + glyph.bearing_x * ratio);
                let y0 = baseline_y + glyph.bearing_y * ratio;
                let x1 = x0 + glyph.width * ratio;
                let y1 = y0 + glyph.height * ratio;

                let u0 = glyph.tex_x;
                let v0 = glyph.tex_y;
                let u1 = glyph.tex_x + glyph.tex_w;
                let v1 = glyph.tex_y + glyph.tex_h;

                vertices.push(TextVertex {
                    position: [x0, y0],
                    tex_coord: [u0, v0],
                    color,
                });
                vertices.push(TextVertex {
                    position: [x1, y0],
                    tex_coord: [u1, v0],
                    color,
                });
                vertices.push(TextVertex {
                    position: [x0, y1],
                    tex_coord: [u0, v1],
                    color,
                });

                vertices.push(TextVertex {
                    position: [x1, y0],
                    tex_coord: [u1, v0],
                    color,
                });
                vertices.push(TextVertex {
                    position: [x1, y1],
                    tex_coord: [u1, v1],
                    color,
                });
                vertices.push(TextVertex {
                    position: [x0, y1],
                    tex_coord: [u0, v1],
                    color,
                });
            }

            cursor_x += glyph.advance * ratio;
            prev_char = Some(resolved_char);
        }

        vertices
    }

    /// Create vertices for a text string
    ///
    /// The y parameter is the TOP of the text line. Text is positioned using
    /// the font's ascent to compute the baseline, ensuring proper alignment
    /// of characters with different heights.
    pub fn layout_text(&self, text: &str, x: f32, y: f32, color: [f32; 4]) -> Vec<TextVertex> {
        self.layout_text_internal(text, x, y, color, 1.0)
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
    pub fn layout_text_small(
        &self,
        text: &str,
        x: f32,
        y: f32,
        color: [f32; 4],
    ) -> Vec<TextVertex> {
        self.layout_text_scaled(text, x, y, color, 0.85)
    }

    /// Layout text at a custom scale factor relative to normal size.
    /// Reuses the existing atlas, rendering smaller or larger quads.
    pub fn layout_text_scaled(
        &self,
        text: &str,
        x: f32,
        y: f32,
        color: [f32; 4],
        text_scale: f32,
    ) -> Vec<TextVertex> {
        self.layout_text_internal(text, x, y, color, text_scale)
    }

    /// Measure the width of a text string at a custom scale factor
    pub fn measure_text_scaled(&self, text: &str, text_scale: f32) -> f32 {
        self.measure_text_internal(text, self.scale_ratio() * text_scale)
    }

    /// Get character width (advance) for a monospace font (scaled to current display)
    pub fn char_width(&self) -> f32 {
        self.glyph_for('M')
            .map(|(_, g)| g.advance * self.scale_ratio())
            .unwrap_or(8.0 * self.render_scale)
    }

    /// Measure the width of a text string in pixels (scaled to current display)
    pub fn measure_text(&self, text: &str) -> f32 {
        self.measure_text_internal(text, self.scale_ratio())
    }

    /// Create a vertex buffer from vertices
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
            .push_constants(
                layout,
                0,
                vs::PushConstants {
                    screen_size: [viewport.extent[0], viewport.extent[1]],
                    sdf_params: [self.sdf_edge_center, self.sdf_aa_scale],
                },
            )
            .context("Failed to push constants")?
            .set_viewport(0, [viewport].into_iter().collect())
            .context("Failed to set viewport")?
            .bind_vertex_buffers(0, vertex_buffer)
            .context("Failed to bind vertex buffers")?;

        // SAFETY: We've bound all required state (pipeline, descriptor sets, vertex buffers)
        // and the vertex count matches the buffer length
        unsafe {
            builder
                .draw(vertex_count, 1, 0, 0)
                .context("Failed to draw")?;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SDF generation from grayscale coverage via contour extraction
// ---------------------------------------------------------------------------

/// Convert a fontdue coverage bitmap into a signed distance field.
///
/// 1. Build a grayscale alpha grid (0..1) from coverage.
/// 2. Extract the 0.5 isocontour using linear interpolation on cell edges.
/// 3. Compute signed distance from each pixel center to nearest contour segment.
/// 4. Encode as SDF: 128 = edge, >128 = inside, <128 = outside.
///
/// Returns `(sdf_bytes, padded_width, padded_height)`.
/// The output is padded by `spread` pixels on each side so the SDF gradient
/// extends beyond the glyph boundary.
fn coverage_to_sdf(
    coverage: &[u8],
    cov_w: usize,
    cov_h: usize,
    spread: usize,
) -> (Vec<u8>, usize, usize) {
    let w = cov_w + 2 * spread;
    let h = cov_h + 2 * spread;
    let size = w * h;
    let iso = 0.5_f32;

    // Build padded grayscale coverage (alpha in [0, 1]).
    let mut alpha = vec![0.0_f32; size];
    for y in 0..cov_h {
        for x in 0..cov_w {
            alpha[(y + spread) * w + (x + spread)] = coverage[y * cov_w + x] as f32 / 255.0;
        }
    }

    #[derive(Clone, Copy)]
    struct Segment {
        a: [f32; 2],
        b: [f32; 2],
    }

    #[inline]
    fn edge_iso_t(a: f32, b: f32, iso: f32) -> Option<f32> {
        let da = a - iso;
        let db = b - iso;
        if (da < 0.0 && db < 0.0) || (da > 0.0 && db > 0.0) {
            return None;
        }
        let denom = b - a;
        if denom.abs() <= 1e-6 {
            return Some(0.5);
        }
        Some(((iso - a) / denom).clamp(0.0, 1.0))
    }

    #[inline]
    fn point_segment_dist_sq(px: f32, py: f32, a: [f32; 2], b: [f32; 2]) -> f32 {
        let vx = b[0] - a[0];
        let vy = b[1] - a[1];
        let wx = px - a[0];
        let wy = py - a[1];
        let vv = vx * vx + vy * vy;
        if vv <= 1e-8 {
            return wx * wx + wy * wy;
        }
        let t = ((wx * vx + wy * vy) / vv).clamp(0.0, 1.0);
        let dx = wx - t * vx;
        let dy = wy - t * vy;
        dx * dx + dy * dy
    }

    // Extract contour segments from the 0.5 isocontour.
    let mut segments = Vec::<Segment>::new();
    for y in 0..h.saturating_sub(1) {
        for x in 0..w.saturating_sub(1) {
            let a00 = alpha[y * w + x];
            let a10 = alpha[y * w + (x + 1)];
            let a01 = alpha[(y + 1) * w + x];
            let a11 = alpha[(y + 1) * w + (x + 1)];

            let mut top = None;
            let mut right = None;
            let mut bottom = None;
            let mut left = None;

            if let Some(t) = edge_iso_t(a00, a10, iso) {
                top = Some([x as f32 + 0.5 + t, y as f32 + 0.5]);
            }
            if let Some(t) = edge_iso_t(a10, a11, iso) {
                right = Some([x as f32 + 1.5, y as f32 + 0.5 + t]);
            }
            if let Some(t) = edge_iso_t(a01, a11, iso) {
                bottom = Some([x as f32 + 0.5 + t, y as f32 + 1.5]);
            }
            if let Some(t) = edge_iso_t(a00, a01, iso) {
                left = Some([x as f32 + 0.5, y as f32 + 0.5 + t]);
            }

            let mut pts = Vec::<[f32; 2]>::with_capacity(4);
            if let Some(p) = top {
                pts.push(p);
            }
            if let Some(p) = right {
                pts.push(p);
            }
            if let Some(p) = bottom {
                pts.push(p);
            }
            if let Some(p) = left {
                pts.push(p);
            }

            match pts.len() {
                2 => {
                    segments.push(Segment {
                        a: pts[0],
                        b: pts[1],
                    });
                }
                4 => {
                    // Ambiguous saddle case: resolve with cell center value.
                    // Center inside -> connect around outside corners.
                    let center = 0.25 * (a00 + a10 + a01 + a11);
                    let (Some(t), Some(r), Some(b), Some(l)) = (top, right, bottom, left) else {
                        continue;
                    };
                    if center >= iso {
                        segments.push(Segment { a: t, b: r });
                        segments.push(Segment { a: b, b: l });
                    } else {
                        segments.push(Segment { a: t, b: l });
                        segments.push(Segment { a: r, b });
                    }
                }
                _ => {}
            }
        }
    }

    let spread_f = spread.max(1) as f32;
    let mut sdf = vec![128u8; size];

    // If we found no contour segments (degenerate input), fall back to alpha-centered signed values.
    if segments.is_empty() {
        for i in 0..size {
            let signed_dist = (alpha[i] - iso).clamp(-0.5, 0.5);
            let val = (signed_dist / spread_f) * 127.0 + 128.0;
            sdf[i] = val.clamp(0.0, 255.0) as u8;
        }
        return (sdf, w, h);
    }

    // Signed distance: positive inside (alpha >= 0.5), negative outside.
    for y in 0..h {
        for x in 0..w {
            let idx = y * w + x;
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let mut min_d2 = f32::INFINITY;
            for seg in &segments {
                min_d2 = min_d2.min(point_segment_dist_sq(px, py, seg.a, seg.b));
            }
            let dist = min_d2.sqrt();
            let signed_dist = if alpha[idx] >= iso { dist } else { -dist };
            let val = (signed_dist / spread_f) * 127.0 + 128.0;
            sdf[idx] = val.clamp(0.0, 255.0) as u8;
        }
    }

    (sdf, w, h)
}
