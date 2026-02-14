//! SplineRenderer for filled shapes and bezier curves.
//!
//! CPU-tessellates lines, cubic bezier curves, rectangles, rounded rects, circles, and arcs into
//! vertex buffers. Used for commit graph lines, backgrounds, borders, and all filled/outlined shapes.

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use std::sync::Arc;
use vulkano::{
    buffer::{Buffer, BufferCreateInfo, BufferUsage, Subbuffer},
    command_buffer::{AutoCommandBufferBuilder, PrimaryAutoCommandBuffer},
    device::DeviceOwned,
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
        GraphicsPipeline, Pipeline, PipelineLayout, PipelineShaderStageCreateInfo,
    },
    render_pass::{RenderPass, Subpass},
};

/// Vertex for spline rendering (position + color, no texture)
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable, Vertex)]
pub struct SplineVertex {
    #[format(R32G32_SFLOAT)]
    pub position: [f32; 2],
    #[format(R32G32B32A32_SFLOAT)]
    pub color: [f32; 4],
}

/// A 2D point
#[derive(Clone, Copy, Debug, Default)]
pub struct SplinePoint {
    pub x: f32,
    pub y: f32,
}

impl SplinePoint {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// A segment of a spline - either a line or a cubic Bezier curve
#[derive(Clone, Debug)]
pub enum SplineSegment {
    /// Linear segment from current point to end
    Line { end: SplinePoint },
    /// Cubic Bezier curve with two control points
    CubicBezier {
        control1: SplinePoint,
        control2: SplinePoint,
        end: SplinePoint,
    },
}

/// A complete spline path with color and width
#[derive(Clone, Debug)]
pub struct Spline {
    pub start: SplinePoint,
    pub segments: Vec<SplineSegment>,
    pub color: [f32; 4],
    pub width: f32,
}

impl Spline {
    pub fn new(start: SplinePoint, color: [f32; 4], width: f32) -> Self {
        Self {
            start,
            segments: Vec::new(),
            color,
            width,
        }
    }

    pub fn line_to(&mut self, end: SplinePoint) {
        self.segments.push(SplineSegment::Line { end });
    }

    pub fn cubic_to(&mut self, control1: SplinePoint, control2: SplinePoint, end: SplinePoint) {
        self.segments.push(SplineSegment::CubicBezier {
            control1,
            control2,
            end,
        });
    }

    /// Tessellate the spline into a series of points
    fn tessellate(&self, segments_per_curve: usize) -> Vec<SplinePoint> {
        let mut points = vec![self.start];
        let mut current = self.start;

        for segment in &self.segments {
            match segment {
                SplineSegment::Line { end } => {
                    points.push(*end);
                    current = *end;
                }
                SplineSegment::CubicBezier {
                    control1,
                    control2,
                    end,
                } => {
                    // Tessellate cubic Bezier: P(t) = (1-t)³P₀ + 3(1-t)²tP₁ + 3(1-t)t²P₂ + t³P₃
                    for i in 1..=segments_per_curve {
                        let t = i as f32 / segments_per_curve as f32;
                        let t2 = t * t;
                        let t3 = t2 * t;
                        let mt = 1.0 - t;
                        let mt2 = mt * mt;
                        let mt3 = mt2 * mt;

                        let x = mt3 * current.x
                            + 3.0 * mt2 * t * control1.x
                            + 3.0 * mt * t2 * control2.x
                            + t3 * end.x;
                        let y = mt3 * current.y
                            + 3.0 * mt2 * t * control1.y
                            + 3.0 * mt * t2 * control2.y
                            + t3 * end.y;

                        points.push(SplinePoint::new(x, y));
                    }
                    current = *end;
                }
            }
        }

        points
    }

    /// Generate triangle vertices for the spline with the specified width
    pub fn to_vertices(&self, segments_per_curve: usize) -> Vec<SplineVertex> {
        let points = self.tessellate(segments_per_curve);
        if points.len() < 2 {
            return Vec::new();
        }

        let half_width = self.width / 2.0;
        let mut strip_left = Vec::with_capacity(points.len());
        let mut strip_right = Vec::with_capacity(points.len());

        for i in 0..points.len() {
            // Compute tangent direction
            let tangent = if i == 0 {
                // First point: use direction to next point
                let dx = points[1].x - points[0].x;
                let dy = points[1].y - points[0].y;
                let len = (dx * dx + dy * dy).sqrt().max(0.0001);
                (dx / len, dy / len)
            } else if i == points.len() - 1 {
                // Last point: use direction from previous point
                let dx = points[i].x - points[i - 1].x;
                let dy = points[i].y - points[i - 1].y;
                let len = (dx * dx + dy * dy).sqrt().max(0.0001);
                (dx / len, dy / len)
            } else {
                // Middle point: average of incoming and outgoing directions
                let dx1 = points[i].x - points[i - 1].x;
                let dy1 = points[i].y - points[i - 1].y;
                let dx2 = points[i + 1].x - points[i].x;
                let dy2 = points[i + 1].y - points[i].y;
                let dx = dx1 + dx2;
                let dy = dy1 + dy2;
                let len = (dx * dx + dy * dy).sqrt().max(0.0001);
                (dx / len, dy / len)
            };

            // Perpendicular direction (rotate 90 degrees)
            let perp = (-tangent.1, tangent.0);

            // Offset vertices
            strip_left.push(SplinePoint::new(
                points[i].x + perp.0 * half_width,
                points[i].y + perp.1 * half_width,
            ));
            strip_right.push(SplinePoint::new(
                points[i].x - perp.0 * half_width,
                points[i].y - perp.1 * half_width,
            ));
        }

        // Convert triangle strip to triangle list
        let mut vertices = Vec::with_capacity((points.len() - 1) * 6);
        for i in 0..points.len() - 1 {
            // First triangle
            vertices.push(SplineVertex {
                position: [strip_left[i].x, strip_left[i].y],
                color: self.color,
            });
            vertices.push(SplineVertex {
                position: [strip_right[i].x, strip_right[i].y],
                color: self.color,
            });
            vertices.push(SplineVertex {
                position: [strip_left[i + 1].x, strip_left[i + 1].y],
                color: self.color,
            });

            // Second triangle
            vertices.push(SplineVertex {
                position: [strip_right[i].x, strip_right[i].y],
                color: self.color,
            });
            vertices.push(SplineVertex {
                position: [strip_right[i + 1].x, strip_right[i + 1].y],
                color: self.color,
            });
            vertices.push(SplineVertex {
                position: [strip_left[i + 1].x, strip_left[i + 1].y],
                color: self.color,
            });
        }

        vertices
    }
}

mod vs {
    vulkano_shaders::shader! {
        ty: "vertex",
        src: r"
            #version 450

            layout(location = 0) in vec2 position;
            layout(location = 1) in vec4 color;

            layout(location = 0) out vec4 v_color;

            layout(push_constant) uniform PushConstants {
                vec2 screen_size;
            } pc;

            void main() {
                // Convert pixel coords to NDC
                vec2 ndc = (position / pc.screen_size) * 2.0 - 1.0;
                gl_Position = vec4(ndc.x, ndc.y, 0.0, 1.0);
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

            layout(location = 0) in vec4 v_color;

            layout(location = 0) out vec4 f_color;

            void main() {
                f_color = v_color;
            }
        ",
    }
}

/// Renderer for splines using triangle strips
pub struct SplineRenderer {
    pipeline: Arc<GraphicsPipeline>,
    memory_allocator: Arc<StandardMemoryAllocator>,
}

impl SplineRenderer {
    pub fn new(
        memory_allocator: Arc<StandardMemoryAllocator>,
        render_pass: Arc<RenderPass>,
    ) -> Result<Self> {
        let device = memory_allocator.device().clone();

        // Create pipeline
        let vs = vs::load(device.clone()).context("Failed to load vertex shader")?;
        let fs = fs::load(device.clone()).context("Failed to load fragment shader")?;

        let vs_entry = vs.entry_point("main").unwrap();
        let fs_entry = fs.entry_point("main").unwrap();

        let vertex_input_state = SplineVertex::per_vertex().definition(&vs_entry).unwrap();

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

        Ok(Self {
            pipeline,
            memory_allocator,
        })
    }

    /// Create a vertex buffer from vertices
    pub fn create_vertex_buffer(
        &self,
        vertices: Vec<SplineVertex>,
    ) -> Result<Subbuffer<[SplineVertex]>> {
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

    /// Draw splines
    pub fn draw(
        &self,
        builder: &mut AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
        vertex_buffer: Subbuffer<[SplineVertex]>,
        viewport: Viewport,
    ) -> Result<()> {
        let layout = self.pipeline.layout().clone();
        let vertex_count = vertex_buffer.len() as u32;

        builder
            .bind_pipeline_graphics(self.pipeline.clone())
            .context("Failed to bind pipeline")?
            .push_constants(
                layout.clone(),
                0,
                vs::PushConstants {
                    screen_size: [viewport.extent[0], viewport.extent[1]],
                },
            )
            .context("Failed to push constants")?
            .set_viewport(0, [viewport].into_iter().collect())
            .context("Failed to set viewport")?
            .bind_vertex_buffers(0, vertex_buffer)
            .context("Failed to bind vertex buffers")?;

        // SAFETY: We've bound all required state (pipeline, vertex buffers)
        // and the vertex count matches the buffer length
        unsafe {
            builder
                .draw(vertex_count, 1, 0, 0)
                .context("Failed to draw")?;
        }

        Ok(())
    }
}
