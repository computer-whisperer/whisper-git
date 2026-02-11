//! Core widget trait and infrastructure
//!
//! Provides a retained-mode widget system where widgets track their own state
//! but regenerate vertices each frame (immediate-mode rendering).

use std::sync::atomic::{AtomicU64, Ordering};

use crate::input::{InputEvent, EventResponse};
use crate::ui::{Rect, SplineVertex, TextRenderer, TextVertex};

/// Unique identifier for widgets
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WidgetId(pub u64);

impl WidgetId {
    /// Generate a new unique widget ID
    pub fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        WidgetId(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for WidgetId {
    fn default() -> Self {
        Self::new()
    }
}

/// Common widget state
#[derive(Clone, Debug, Default)]
pub struct WidgetState {
    /// Whether the widget is hovered
    pub hovered: bool,
    /// Whether the widget is focused (receives keyboard input)
    pub focused: bool,
    /// Whether the widget is currently pressed
    pub pressed: bool,
    /// Whether the widget is enabled (can receive input)
    pub enabled: bool,
}

impl WidgetState {
    pub fn new() -> Self {
        Self {
            hovered: false,
            focused: false,
            pressed: false,
            enabled: true,
        }
    }
}

/// Output from a widget's layout pass
pub struct WidgetOutput {
    /// Vertices for spline/shape rendering
    pub spline_vertices: Vec<SplineVertex>,
    /// Vertices for text rendering (regular weight)
    pub text_vertices: Vec<TextVertex>,
    /// Vertices for bold text rendering (separate font atlas)
    pub bold_text_vertices: Vec<TextVertex>,
    /// Vertices for avatar rendering (separate texture atlas)
    pub avatar_vertices: Vec<TextVertex>,
}

impl WidgetOutput {
    pub fn new() -> Self {
        Self {
            spline_vertices: Vec::new(),
            text_vertices: Vec::new(),
            bold_text_vertices: Vec::new(),
            avatar_vertices: Vec::new(),
        }
    }

    pub fn extend(&mut self, other: WidgetOutput) {
        self.spline_vertices.extend(other.spline_vertices);
        self.text_vertices.extend(other.text_vertices);
        self.bold_text_vertices.extend(other.bold_text_vertices);
        self.avatar_vertices.extend(other.avatar_vertices);
    }
}

impl Default for WidgetOutput {
    fn default() -> Self {
        Self::new()
    }
}

/// The core widget trait
#[allow(dead_code)]
pub trait Widget {
    /// Get this widget's unique ID
    fn id(&self) -> WidgetId;

    /// Handle an input event, returning whether it was consumed
    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        let _ = (event, bounds);
        EventResponse::Ignored
    }

    /// Update hover state based on mouse position
    fn update_hover(&mut self, x: f32, y: f32, bounds: Rect) {
        let _ = (x, y, bounds);
    }

    /// Layout the widget and produce rendering output
    fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput;

    /// Check if this widget can receive focus
    fn focusable(&self) -> bool {
        false
    }

    /// Set focus state
    fn set_focused(&mut self, focused: bool) {
        let _ = focused;
    }

    /// Get focus state
    fn is_focused(&self) -> bool {
        false
    }

    /// Get the widget's preferred size (for layout calculations)
    fn preferred_size(&self, text_renderer: &TextRenderer) -> (f32, f32) {
        let _ = text_renderer;
        (0.0, 0.0)
    }
}

/// Helper to create filled rectangle vertices
pub fn create_rect_vertices(rect: &Rect, color: [f32; 4]) -> Vec<SplineVertex> {
    let x0 = rect.x;
    let y0 = rect.y;
    let x1 = rect.right();
    let y1 = rect.bottom();

    vec![
        // First triangle
        SplineVertex { position: [x0, y0], color },
        SplineVertex { position: [x1, y0], color },
        SplineVertex { position: [x0, y1], color },
        // Second triangle
        SplineVertex { position: [x1, y0], color },
        SplineVertex { position: [x1, y1], color },
        SplineVertex { position: [x0, y1], color },
    ]
}

/// Helper to create filled rounded rectangle vertices
///
/// Generates triangles for a rectangle with quarter-circle corner arcs.
/// Uses center body + 2 side strips + 4 corner fans (adaptive segments per corner).
pub fn create_rounded_rect_vertices(rect: &Rect, color: [f32; 4], radius: f32) -> Vec<SplineVertex> {
    let r = radius.min(rect.width / 2.0).min(rect.height / 2.0);
    if r < 0.5 {
        return create_rect_vertices(rect, color);
    }

    let mut vertices = Vec::new();

    // Central rectangle (full width, excluding top/bottom corner rows)
    vertices.extend(create_rect_vertices(
        &Rect::new(rect.x + r, rect.y, rect.width - 2.0 * r, rect.height),
        color,
    ));
    // Left strip (between corners)
    vertices.extend(create_rect_vertices(
        &Rect::new(rect.x, rect.y + r, r, rect.height - 2.0 * r),
        color,
    ));
    // Right strip (between corners)
    vertices.extend(create_rect_vertices(
        &Rect::new(rect.right() - r, rect.y + r, r, rect.height - 2.0 * r),
        color,
    ));

    // Corner arcs (quarter circles): (center_x, center_y, start_angle, end_angle)
    let corners = [
        (rect.x + r, rect.y + r, std::f32::consts::PI, std::f32::consts::FRAC_PI_2 * 3.0),
        (rect.right() - r, rect.y + r, std::f32::consts::FRAC_PI_2 * 3.0, std::f32::consts::TAU),
        (rect.right() - r, rect.bottom() - r, 0.0, std::f32::consts::FRAC_PI_2),
        (rect.x + r, rect.bottom() - r, std::f32::consts::FRAC_PI_2, std::f32::consts::PI),
    ];

    // Adaptive segment count: more segments for larger radii to keep corners smooth.
    // At 6 segments each step is 15 deg — visibly faceted. At 8+ it's much smoother.
    let segments = ((r * 1.5).ceil() as usize).clamp(8, 16);
    for (cx, cy, start_angle, end_angle) in corners {
        for i in 0..segments {
            let a1 = start_angle + (end_angle - start_angle) * (i as f32 / segments as f32);
            let a2 = start_angle + (end_angle - start_angle) * ((i + 1) as f32 / segments as f32);
            vertices.push(SplineVertex { position: [cx, cy], color });
            vertices.push(SplineVertex { position: [cx + r * a1.cos(), cy + r * a1.sin()], color });
            vertices.push(SplineVertex { position: [cx + r * a2.cos(), cy + r * a2.sin()], color });
        }
    }

    vertices
}

/// Helper to create rounded rectangle outline vertices (border only, no fill).
///
/// Draws the gap between an outer rounded rect and an inner rounded rect (inset
/// by `thickness`) using 4 straight edge rectangles and 4 corner arc strips.
pub fn create_rounded_rect_outline_vertices(
    rect: &Rect,
    color: [f32; 4],
    radius: f32,
    thickness: f32,
) -> Vec<SplineVertex> {
    let r = radius.min(rect.width / 2.0).min(rect.height / 2.0);
    if r < 0.5 {
        return create_rect_outline_vertices(rect, color, thickness);
    }

    let mut vertices = Vec::new();
    let ri = (r - thickness).max(0.0); // inner corner radius

    // Top edge (between top-left and top-right corners)
    vertices.extend(create_rect_vertices(
        &Rect::new(rect.x + r, rect.y, rect.width - 2.0 * r, thickness),
        color,
    ));
    // Bottom edge
    vertices.extend(create_rect_vertices(
        &Rect::new(rect.x + r, rect.bottom() - thickness, rect.width - 2.0 * r, thickness),
        color,
    ));
    // Left edge (between top-left and bottom-left corners)
    vertices.extend(create_rect_vertices(
        &Rect::new(rect.x, rect.y + r, thickness, rect.height - 2.0 * r),
        color,
    ));
    // Right edge
    vertices.extend(create_rect_vertices(
        &Rect::new(rect.right() - thickness, rect.y + r, thickness, rect.height - 2.0 * r),
        color,
    ));

    // Corner arcs: thin triangle strips between outer radius and inner radius
    let corners = [
        (rect.x + r, rect.y + r, std::f32::consts::PI, std::f32::consts::FRAC_PI_2 * 3.0),
        (rect.right() - r, rect.y + r, std::f32::consts::FRAC_PI_2 * 3.0, std::f32::consts::TAU),
        (rect.right() - r, rect.bottom() - r, 0.0, std::f32::consts::FRAC_PI_2),
        (rect.x + r, rect.bottom() - r, std::f32::consts::FRAC_PI_2, std::f32::consts::PI),
    ];

    // Adaptive segment count: match fill function for consistent corner smoothness.
    let segments = ((r * 1.5).ceil() as usize).clamp(8, 16);
    for (cx, cy, start_angle, end_angle) in corners {
        for i in 0..segments {
            let a1 = start_angle + (end_angle - start_angle) * (i as f32 / segments as f32);
            let a2 = start_angle + (end_angle - start_angle) * ((i + 1) as f32 / segments as f32);

            let outer1 = [cx + r * a1.cos(), cy + r * a1.sin()];
            let outer2 = [cx + r * a2.cos(), cy + r * a2.sin()];
            let inner1 = [cx + ri * a1.cos(), cy + ri * a1.sin()];
            let inner2 = [cx + ri * a2.cos(), cy + ri * a2.sin()];

            // Two triangles for each arc segment
            vertices.push(SplineVertex { position: outer1, color });
            vertices.push(SplineVertex { position: outer2, color });
            vertices.push(SplineVertex { position: inner1, color });

            vertices.push(SplineVertex { position: outer2, color });
            vertices.push(SplineVertex { position: inner2, color });
            vertices.push(SplineVertex { position: inner1, color });
        }
    }

    vertices
}

/// Helper to create rectangle outline vertices
pub fn create_rect_outline_vertices(rect: &Rect, color: [f32; 4], thickness: f32) -> Vec<SplineVertex> {
    let mut vertices = Vec::new();

    // Top edge
    vertices.extend(create_rect_vertices(
        &Rect::new(rect.x, rect.y, rect.width, thickness),
        color,
    ));

    // Bottom edge
    vertices.extend(create_rect_vertices(
        &Rect::new(rect.x, rect.bottom() - thickness, rect.width, thickness),
        color,
    ));

    // Left edge
    vertices.extend(create_rect_vertices(
        &Rect::new(rect.x, rect.y + thickness, thickness, rect.height - 2.0 * thickness),
        color,
    ));

    // Right edge
    vertices.extend(create_rect_vertices(
        &Rect::new(rect.right() - thickness, rect.y + thickness, thickness, rect.height - 2.0 * thickness),
        color,
    ));

    vertices
}

/// Theme colors - Dark blue-gray palette
pub mod theme {
    use crate::ui::Color;

    // Dark blue-gray palette
    pub const BACKGROUND: Color = Color::rgba(0.102, 0.118, 0.141, 1.0);      // #1a1e24 - dark blue-gray
    pub const SURFACE: Color = Color::rgba(0.133, 0.149, 0.173, 1.0);         // #222630 - panels
    pub const SURFACE_RAISED: Color = Color::rgba(0.165, 0.188, 0.251, 1.0);  // #2a3040 - elevated elements
    pub const SURFACE_HOVER: Color = Color::rgba(0.200, 0.224, 0.290, 1.0);   // #33394a - hover states
    pub const BORDER: Color = Color::rgba(0.227, 0.227, 0.251, 1.0);          // #3a3a40 - subtle borders
    pub const BORDER_LIGHT: Color = Color::rgba(0.282, 0.282, 0.306, 1.0);    // #48484e - emphasized borders
    pub const TEXT: Color = Color::rgba(0.878, 0.878, 0.878, 1.0);            // #e0e0e0 - primary text
    pub const TEXT_BRIGHT: Color = Color::rgba(0.940, 0.940, 0.940, 1.0);     // #f0f0f0 - emphasized text
    pub const TEXT_MUTED: Color = Color::rgba(0.635, 0.635, 0.635, 1.0);      // #a2a2a2 - secondary text (brightened for contrast)

    // Status colors - slightly desaturated for dark theme
    pub const STATUS_CLEAN: Color = Color::rgba(0.298, 0.686, 0.314, 1.0);    // #4CAF50 (Green)
    pub const STATUS_BEHIND: Color = Color::rgba(1.000, 0.596, 0.000, 1.0);   // #FF9800 (Orange)
    pub const STATUS_DIRTY: Color = Color::rgba(0.937, 0.325, 0.314, 1.0);    // #EF5350 (Red)
    pub const STATUS_AHEAD: Color = Color::rgba(0.259, 0.647, 0.961, 1.0);    // #42A5F5 (Blue)

    // Branch colors - vibrant but balanced
    pub const BRANCH_FEATURE: Color = Color::rgba(0.298, 0.686, 0.314, 1.0);  // #4CAF50 (Green)
    pub const BRANCH_RELEASE: Color = Color::rgba(1.000, 0.596, 0.000, 1.0);  // #FF9800 (Orange)
    pub const BRANCH_REMOTE: Color = Color::rgba(0.620, 0.620, 0.620, 1.0);   // #9e9e9e (Gray)

    // Accent color for selections and focus
    pub const ACCENT: Color = Color::rgba(0.259, 0.647, 0.961, 1.0);          // #42A5F5 (Blue)
    pub const ACCENT_MUTED: Color = Color::rgba(0.259, 0.647, 0.961, 0.3);    // Blue at 30% opacity

    // Panel depth - dark blue-gray hierarchy
    pub const PANEL_SIDEBAR: Color = Color::rgba(0.075, 0.090, 0.110, 1.0);   // #131720 - sidebar (darkest)
    pub const PANEL_GRAPH: Color = Color::rgba(0.102, 0.118, 0.141, 1.0);     // #1a1e24 - graph (base)
    pub const PANEL_STAGING: Color = Color::rgba(0.122, 0.141, 0.188, 1.0);   // #1f2430 - staging (slightly lighter)

    // Zebra striping for graph rows
    pub const GRAPH_ROW_ALT: Color = Color::rgba(0.145, 0.165, 0.204, 1.0);   // #252a34 - increased zebra stripe contrast (~5-6% luminance diff)

    /// Lane colors for visual distinction in the commit graph
    pub const LANE_COLORS: &[Color] = &[
        Color::rgba(0.231, 0.510, 0.965, 1.0), // Blue - primary branch
        Color::rgba(0.133, 0.773, 0.369, 1.0), // Green - feature branches
        Color::rgba(0.961, 0.620, 0.043, 1.0), // Amber - release branches
        Color::rgba(0.659, 0.333, 0.969, 1.0), // Purple - hotfix branches
        Color::rgba(0.392, 0.455, 0.545, 1.0), // Slate - remote tracking
        Color::rgba(0.4, 0.9, 0.9, 1.0),       // Cyan
        Color::rgba(1.0, 0.5, 0.5, 1.0),       // Red
        Color::rgba(0.7, 0.7, 0.9, 1.0),       // Lavender
    ];
}
