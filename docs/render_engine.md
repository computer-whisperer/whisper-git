# Render Engine Architecture

Whisper Git uses Vulkano (Rust Vulkan bindings) for GPU-accelerated rendering. This document describes the rendering architecture and how to extend it.

## Overview

```
+-------------------------------------------------------------+
|                        Application                           |
|  +---------+  +---------+  +---------+                       |
|  |   Git   |  |  Input  |  |  Views  |                       |
|  +---------+  +----+----+  +----+----+                       |
|                    |            |                             |
|                    v            v                             |
|  +---------------------------------------+                   |
|  |              Widgets                   |                   |
|  |  +------------+  +---------------+    |                   |
|  |  | WidgetState|  | handle_event  |    |                   |
|  |  +------------+  +---------------+    |                   |
|  +----------------+----------------------+                   |
|                   |  layout() -> WidgetOutput                |
|                   v                                          |
|  +---------------------------------------+                   |
|  |          Vertex Generation            |                   |
|  |  (TextVertex, SplineVertex,           |                   |
|  |   AvatarVertex)                       |                   |
|  +----------------+----------------------+                   |
|                   |                                          |
|                   v                                          |
|  +---------------------------------------+                   |
|  |            Renderers                   |                   |
|  |  +--------+ +--------+ +-----------+  |                   |
|  |  |Spline  | |Avatar  | |  Text     |  |                   |
|  |  |Renderer| |Renderer| | Renderer  |  |                   |
|  |  +--------+ +--------+ +-----------+  |                   |
|  |  +-----------+  +-----------------+   |                   |
|  |  |  Context  |  | SurfaceManager  |   |                   |
|  |  +-----------+  +-----------------+   |                   |
|  +----------------+----------------------+                   |
|                   |                                          |
|                   v                                          |
|  +---------------------------------------+                   |
|  |              Vulkan                    |                   |
|  +---------------------------------------+                   |
+-------------------------------------------------------------+
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
   - Atlas is built at the maximum monitor scale factor for crisp text at all DPIs

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
   - Cubic Bezier curves use the formula: `P(t) = (1-t)^3*P0 + 3(1-t)^2*t*P1 + 3(1-t)*t^2*P2 + t^3*P3`
   - Curves are subdivided into configurable number of line segments (default: 16)

3. **Triangle Strip Generation**:
   - For each tessellated point, compute tangent direction
   - Generate perpendicular offset vertices at +/-half_width
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

**Also Used For:** Rectangles, borders, panel backgrounds, diff highlight bars, scrollbar thumbs, and other filled or outlined shapes. The `create_rect_vertices()` and `create_rect_outline_vertices()` helpers generate `SplineVertex` quads for these.

**Usage Example:**
```rust
let mut spline = Spline::new(SplinePoint::new(0.0, 0.0), color, 2.0);
spline.line_to(SplinePoint::new(100.0, 0.0));
spline.cubic_to(ctrl1, ctrl2, SplinePoint::new(100.0, 100.0));
let vertices = spline.to_vertices(16);
```

### AvatarRenderer (`ui/avatar.rs`)

Renders Gravatar avatar images using a dedicated GPU texture atlas. Introduced in Sprint 11.

**How it works:**

1. **Avatar Cache** (`AvatarCache`):
   - Maintains a map of email -> download state
   - Background threads download Gravatar images via HTTP (`ureq`)
   - Email addresses are MD5-hashed to form the Gravatar URL
   - Downloads are cached to disk at `~/.cache/whisper-git/avatars/`
   - Falls back to on-disk cache if network is unavailable

2. **GPU Atlas**:
   - 512x512 pixel texture divided into an 8x8 grid of 64x64 avatar tiles
   - Supports up to 64 unique avatars per session
   - Atlas uploads happen before the render pass via `upload_atlas()`

3. **Rendering**:
   - Reuses the `TextVertex` format (position + UV + color)
   - Has its own GLSL shaders and graphics pipeline (separate from text)
   - UVs map into the avatar atlas grid rather than the font atlas

**Vertex Format:** Same as `TextVertex` (shared struct, different atlas texture).

### Widget System (`ui/widget.rs`)

The widget system provides the abstraction layer between UI components and rendering:

```rust
pub trait Widget {
    fn id(&self) -> WidgetId;
    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse;
    fn update_hover(&mut self, x: f32, y: f32, bounds: Rect);
    fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput;
}

pub struct WidgetOutput {
    pub spline_vertices: Vec<SplineVertex>,    // Shapes, borders, backgrounds
    pub text_vertices: Vec<TextVertex>,         // Text content
    pub avatar_vertices: Vec<TextVertex>,       // Avatar images
}
```

**How widgets integrate with rendering:**

1. **Event handling**: Widgets receive `InputEvent` (keyboard, mouse) and return `EventResponse::Consumed` or `EventResponse::Ignored`
2. **Hover updates**: Mouse position updates widget hover/focus states
3. **Layout**: Widgets generate `WidgetOutput` containing spline vertices (shapes), text vertices (text), and avatar vertices (images)
4. **Rendering**: Outputs are collected and rendered in three passes

**Widget State:**
```rust
pub struct WidgetState {
    pub hovered: bool,
    pub focused: bool,
    pub pressed: bool,
    pub enabled: bool,
}
```

**Theme Colors** (`ui/widget.rs::theme`):
```rust
BACKGROUND:     #0F172A   // Dark blue background
SURFACE:        #1E293B   // Lighter surface
SURFACE_RAISED: #283548   // Elevated panels (header, dialogs)
SURFACE_HOVER:  #334155   // Hover state
BORDER:         #334155   // Slate borders
BORDER_LIGHT:   #475569   // Lighter borders for emphasis
TEXT:           #F8FAFC   // Near-white text
TEXT_BRIGHT:    #FFFFFF   // Full white (active items)
TEXT_MUTED:     #94A3B8   // Muted text (secondary info)
ACCENT:         #3B82F6   // Blue accent (focused panel, active items)
PANEL_GRAPH:              // Graph panel background
PANEL_STAGING:            // Staging panel background
PANEL_SIDEBAR:            // Sidebar panel background
```

**Lane Colors:** 8-color palette for commit graph visual distinction:
blue, green, orange, purple, yellow, cyan, red, lavender

## Render Loop

Each frame follows this sequence:

```
 1. Cleanup finished GPU work
 2. Recreate swapchain if needed (resize)
 3. Acquire next swapchain image
 4. Sync widget state:
    - Update button labels/styles (header bar, staging well)
    - Update cursor blink state (text inputs, search bar)
    - Set shortcut bar context based on focused panel
 5. Update toasts (tick timers, dismiss expired)
 6. Poll avatar downloads, pack newly loaded images into GPU atlas
 7. Build UI output (build_ui_output):
    a. Panel chrome (backgrounds + borders)
    b. Commit graph (splines, pills, text, avatars) -- rendered first
    c. Tab bar (rendered on top of graph)
    d. Header bar (rendered on top of graph)
    e. Shortcut bar
    f. Branch sidebar
    g. Staging well
    h. Right panel (commit detail / diff view / secondary repos)
    i. Context menu overlay (if visible)
    j. Toast notifications overlay
    k. Repo dialog overlay (if visible)
    l. Settings dialog overlay (if visible)
 8. Upload avatar atlas to GPU (if dirty)
 9. Begin render pass (clear to BACKGROUND color)
10. Draw pass 1: Splines (shapes, backgrounds, graph lines, borders)
11. Draw pass 2: Avatars (Gravatar images from atlas)
12. Draw pass 3: Text (all text content from font atlas)
13. End render pass
14. Submit command buffer
15. Present swapchain image
```

**Z-ordering note:** The graph is rendered first in the UI output build order so that all chrome (header, shortcut bar, sidebar, tab bar) draws on top of graph content. This prevents graph connection lines from visually overlapping the header or other panels.

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

4. **Integrate** into the render loop in `main.rs` (add a new draw pass between existing passes)

### Adding a New Widget

1. Create `ui/widgets/my_widget.rs`
2. Implement the `Widget` trait:
   ```rust
   impl Widget for MyWidget {
       fn id(&self) -> WidgetId { self.id }

       fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
           // Handle input, return Consumed or Ignored
       }

       fn update_hover(&mut self, x: f32, y: f32, bounds: Rect) {
           // Update hover state
       }

       fn layout(&self, text: &TextRenderer, bounds: Rect) -> WidgetOutput {
           // Generate vertices for rendering
       }
   }
   ```
3. Add to `ui/widgets/mod.rs`
4. Use within a view or compose into larger widgets

**Important:** Never create fresh widget instances in `layout()`. Always use stored widget fields. Creating `Button::new()` in layout discards hover/press state tracked by `handle_event()`. Use `update_button_state()` to sync labels before layout instead.

### Adding a New View

1. Create `views/my_view.rs`
2. For simple views, implement layout functions:
   ```rust
   pub fn layout_splines(&self, data: &Data, bounds: Rect) -> Vec<SplineVertex>
   pub fn layout_text(&self, text: &TextRenderer, data: &Data, bounds: Rect) -> Vec<TextVertex>
   ```
3. For interactive views, implement `Widget` or compose multiple widgets:
   ```rust
   pub struct MyView {
       widgets: Vec<Box<dyn Widget>>,
   }

   impl Widget for MyView {
       fn layout(&self, text: &TextRenderer, bounds: Rect) -> WidgetOutput {
           // Combine outputs from child widgets
       }
   }
   ```
4. Add to `views/mod.rs`
5. Integrate into `main.rs` layout and event handling

### CommitGraphView (`views/commit_graph.rs`)

Visualizes Git commit history as a graph with branch lanes:

```rust
pub struct CommitGraphView {
    layout: GraphLayout,      // Lane assignments
    line_width: f32,          // Spline thickness
    lane_width: f32,          // Horizontal spacing between lanes
    row_height: f32,          // Vertical spacing between commits
    node_radius: f32,         // Commit node circle size (24-segment circles)
    search_bar: SearchBar,    // Ctrl+F search/filter
    scrollbar: Scrollbar,     // Vertical scrollbar
    // ...
}
```

**GraphLayout Algorithm:**

1. Process commits in topological order
2. For each commit:
   - Check if any lane is waiting for this commit (as a parent)
   - If not, find the lowest free lane (`lowest_free_lane()`) or allocate a new one
3. Update lanes based on parents:
   - First parent continues in the same lane
   - Additional parents (merges) get assigned to other lanes
4. Lanes are freed when commits are fully processed, enabling reuse and keeping the graph compact

**Connection Types:**
- **Vertical lines**: Commit continues to parent in same lane
- **Bezier curves**: Merge/fork connections across lanes (smooth S-curves)

**Row Layout:**
Each commit row displays: graph lanes | commit subject (prominent) | author name (dimmed) | relative time (right-aligned)

**Lane Colors:**
8-color palette for visual distinction: blue, green, orange, purple, yellow, cyan, red, lavender

## Performance Considerations

### Current Approach
- Vertices rebuilt every frame (simple immediate-mode pattern)
- Single draw call per primitive type (three total: splines, avatars, text)
- No batching across different textures
- Continuous redraw via `about_to_wait()` calling `request_redraw()` for smooth animations

### Future Optimizations
- Cache vertex buffers when content doesn't change
- Use staging buffers for large updates
- Batch multiple primitive types into single draw calls
- Consider compute shaders for complex layouts

## Screenshot Capture

`renderer/screenshot.rs` handles capturing the rendered frame:

1. Copy swapchain image to host-visible buffer
2. Handle format conversion (F16 -> sRGB on AMD GPUs)
3. Return `image::RgbaImage` for PNG encoding

Offscreen rendering (`renderer/offscreen.rs`) supports capturing at arbitrary resolutions without needing a matching window size.

This enables `--screenshot` mode for CI and LLM-assisted development. Screenshot states (`--screenshot-state`) can inject specific UI configurations (open dialog, search bar, context menu, commit detail) before capture.

## Swapchain Formats

Different GPUs use different swapchain formats:

| GPU | Typical Format |
|-----|----------------|
| AMD | R16G16B16A16_SFLOAT |
| NVIDIA | B8G8R8A8_SRGB |
| Intel | B8G8R8A8_UNORM |

Screenshot capture handles these automatically via format-aware conversion. The `half` crate is used for f16 conversion on AMD swapchain formats.
