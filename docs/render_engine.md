# Render Engine Architecture

Whisper Git uses Vulkano (Rust Vulkan bindings) for GPU-accelerated rendering. This document describes the rendering architecture and how to extend it.

## Overview

```
┌─────────────────────────────────────────────────────────────┐
│                        Application                          │
│  ┌─────────┐  ┌─────────┐  ┌─────────┐                     │
│  │  Views  │  │   UI    │  │   Git   │                     │
│  └────┬────┘  └────┬────┘  └─────────┘                     │
│       │            │                                        │
│       ▼            ▼                                        │
│  ┌─────────────────────────────────────┐                   │
│  │          Vertex Generation          │                   │
│  │     (TextVertex, SplineVertex)      │                   │
│  └────────────────┬────────────────────┘                   │
│                   │                                         │
│                   ▼                                         │
│  ┌─────────────────────────────────────┐                   │
│  │            Renderer                  │                   │
│  │  ┌───────────┐  ┌─────────────────┐ │                   │
│  │  │  Context  │  │ SurfaceManager  │ │                   │
│  │  └───────────┘  └─────────────────┘ │                   │
│  └────────────────┬────────────────────┘                   │
│                   │                                         │
│                   ▼                                         │
│  ┌─────────────────────────────────────┐                   │
│  │              Vulkan                  │                   │
│  └─────────────────────────────────────┘                   │
└─────────────────────────────────────────────────────────────┘
```

## Core Components

### VulkanContext (`renderer/context.rs`)

Owns the fundamental Vulkan objects created once at startup:

```rust
pub struct VulkanContext {
    pub instance: Arc<Instance>,
    pub device: Arc<Device>,
    pub queue: Arc<Queue>,
    pub memory_allocator: Arc<StandardMemoryAllocator>,
    pub command_buffer_allocator: Arc<StandardCommandBufferAllocator>,
}
```

**Responsibilities:**
- GPU device selection (prefers discrete GPU)
- Memory allocation for buffers and images
- Command buffer allocation

### SurfaceManager (`renderer/surface.rs`)

Manages the swapchain and framebuffers, which need recreation on window resize:

```rust
pub struct SurfaceManager {
    pub surface: Arc<Surface>,
    pub swapchain: Arc<Swapchain>,
    pub images: Vec<Arc<Image>>,
    pub framebuffers: Vec<Arc<Framebuffer>>,
    pub render_pass: Arc<RenderPass>,
    pub needs_recreate: bool,
}
```

**Responsibilities:**
- Swapchain creation and recreation
- Framebuffer management
- Handles `VK_ERROR_OUT_OF_DATE` gracefully

### TextRenderer (`ui/text.rs`)

Font atlas-based text rendering using ab_glyph for glyph rasterization:

```rust
pub struct TextRenderer {
    pipeline: Arc<GraphicsPipeline>,
    font_texture: Arc<ImageView>,
    sampler: Arc<Sampler>,
    glyphs: HashMap<char, GlyphInfo>,
    // ...
}
```

**How it works:**

1. **Font Atlas Creation** (startup):
   - Loads DejaVu Sans Mono from system fonts
   - Rasterizes ASCII characters (32-126) to a texture atlas
   - Stores glyph metrics (advance, bearing, UV coordinates)

2. **Text Layout** (`layout_text`):
   - Converts string + position to `Vec<TextVertex>`
   - Each character becomes a textured quad (6 vertices, 2 triangles)

3. **Rendering** (`draw`):
   - Binds font atlas texture
   - Sets viewport via push constants
   - Draws all text quads in one draw call

**Vertex Format:**
```rust
pub struct TextVertex {
    pub position: [f32; 2],   // Screen-space pixels
    pub tex_coord: [f32; 2],  // Font atlas UV
    pub color: [f32; 4],      // RGBA
}
```

**Shaders:**
- Vertex: Converts pixel coordinates to NDC using push constant screen size
- Fragment: Samples font atlas alpha, multiplies by vertex color

### SplineRenderer (`ui/spline.rs`)

Renders lines and Bezier curves with consistent thickness using CPU-tessellated triangle strips:

```rust
pub struct SplineRenderer {
    pipeline: Arc<GraphicsPipeline>,
    memory_allocator: Arc<StandardMemoryAllocator>,
}
```

**How it works:**

1. **Spline Definition**:
   - `Spline` contains a start point, segments (Line or CubicBezier), color, and width
   - Segments can be chained to form complex paths

2. **Bezier Tessellation** (`to_vertices`):
   - Cubic Bezier curves use the formula: `P(t) = (1-t)³P₀ + 3(1-t)²tP₁ + 3(1-t)t²P₂ + t³P₃`
   - Curves are subdivided into configurable number of line segments (default: 16)

3. **Triangle Strip Generation**:
   - For each tessellated point, compute tangent direction
   - Generate perpendicular offset vertices at ±half_width
   - Convert strip to triangle list for rendering

**Vertex Format:**
```rust
pub struct SplineVertex {
    pub position: [f32; 2],   // Screen-space pixels
    pub color: [f32; 4],      // RGBA
}
```

**Shaders:**
- Vertex: Converts pixel coordinates to NDC using push constant screen size
- Fragment: Outputs solid vertex color (no texture sampling)

**Usage Example:**
```rust
let mut spline = Spline::new(SplinePoint::new(0.0, 0.0), color, 2.0);
spline.line_to(SplinePoint::new(100.0, 0.0));
spline.cubic_to(ctrl1, ctrl2, SplinePoint::new(100.0, 100.0));
let vertices = spline.to_vertices(16);
```

## Render Loop

Each frame follows this sequence:

```
1. Cleanup finished GPU work
2. Recreate swapchain if needed (resize)
3. Acquire next swapchain image
4. Build UI vertices:
   - Views generate spline vertices (lines, curves)
   - Views generate text vertices (labels)
5. Begin render pass (clear to background color)
6. Draw splines (background layer)
7. Draw text (foreground layer)
8. End render pass
9. Submit command buffer
10. Present swapchain image
```

## Adding New Rendering Features

### Adding a New Primitive (e.g., colored quads)

1. **Define vertex type** in `ui/`:
   ```rust
   #[derive(Vertex)]
   pub struct QuadVertex {
       pub position: [f32; 2],
       pub color: [f32; 4],
   }
   ```

2. **Create pipeline** with appropriate shaders

3. **Add renderer** similar to TextRenderer:
   - `layout_quad(rect, color) -> Vec<QuadVertex>`
   - `draw(builder, vertices, viewport)`

4. **Integrate** into the render loop in `main.rs`

### Adding a New View

1. Create `views/my_view.rs`
2. Implement layout functions:
   ```rust
   pub fn layout_splines(&self, data: &Data, bounds: Rect) -> Vec<SplineVertex>
   pub fn layout_text(&self, text: &TextRenderer, data: &Data, bounds: Rect) -> Vec<TextVertex>
   ```
3. Add to `views/mod.rs`
4. Use in `main.rs` draw_frame

### CommitGraphView (`views/commit_graph.rs`)

Visualizes Git commit history as a graph with branch lanes:

```rust
pub struct CommitGraphView {
    layout: GraphLayout,      // Lane assignments
    line_width: f32,          // Spline thickness
    lane_width: f32,          // Horizontal spacing between lanes
    row_height: f32,          // Vertical spacing between commits
    node_radius: f32,         // Commit node circle size
}
```

**GraphLayout Algorithm:**

1. Process commits in topological order
2. For each commit:
   - Check if any lane is waiting for this commit (as a parent)
   - If not, find an empty lane or create a new one
3. Update lanes based on parents:
   - First parent continues in the same lane
   - Additional parents (merges) get assigned to other lanes
4. Empty lanes can be reused

**Connection Types:**
- **Vertical lines**: Commit continues to parent in same lane
- **Bezier curves**: Merge/fork connections across lanes (smooth S-curves)

**Lane Colors:**
8-color palette for visual distinction: blue, green, orange, purple, yellow, cyan, red, lavender

## Performance Considerations

### Current Approach
- Vertices rebuilt every frame (simple, but not optimal)
- Single draw call per primitive type
- No batching across different textures

### Future Optimizations
- Cache vertex buffers when content doesn't change
- Use staging buffers for large updates
- Batch multiple primitive types into single draw calls
- Consider compute shaders for complex layouts

## Screenshot Capture

`renderer/screenshot.rs` handles capturing the rendered frame:

1. Copy swapchain image to host-visible buffer
2. Handle format conversion (F16 → sRGB on AMD GPUs)
3. Return `image::RgbaImage` for PNG encoding

This enables `--screenshot` mode for CI and LLM-assisted development.

## Swapchain Formats

Different GPUs use different swapchain formats:

| GPU | Typical Format |
|-----|----------------|
| AMD | R16G16B16A16_SFLOAT |
| NVIDIA | B8G8R8A8_SRGB |
| Intel | B8G8R8A8_UNORM |

Screenshot capture handles these automatically via format-aware conversion.
