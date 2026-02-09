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
    /// Vertices for text rendering
    pub text_vertices: Vec<TextVertex>,
    /// Vertices for avatar rendering (separate texture atlas)
    pub avatar_vertices: Vec<TextVertex>,
}

impl WidgetOutput {
    pub fn new() -> Self {
        Self {
            spline_vertices: Vec::new(),
            text_vertices: Vec::new(),
            avatar_vertices: Vec::new(),
        }
    }

    pub fn extend(&mut self, other: WidgetOutput) {
        self.spline_vertices.extend(other.spline_vertices);
        self.text_vertices.extend(other.text_vertices);
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

/// Helper to create dashed rectangle outline vertices
pub fn create_dashed_rect_outline_vertices(
    rect: &Rect,
    color: [f32; 4],
    thickness: f32,
    dash_length: f32,
    gap_length: f32,
) -> Vec<SplineVertex> {
    let mut vertices = Vec::new();

    // Helper to create dashes along a line
    let create_dashes = |x0: f32, y0: f32, x1: f32, y1: f32, is_horizontal: bool| -> Vec<SplineVertex> {
        let mut dash_vertices = Vec::new();
        let length = if is_horizontal { (x1 - x0).abs() } else { (y1 - y0).abs() };
        let segment_length = dash_length + gap_length;
        let num_segments = (length / segment_length).ceil() as i32;

        for i in 0..num_segments {
            let start = i as f32 * segment_length;
            let end = (start + dash_length).min(length);

            if is_horizontal {
                let dash_x0 = x0.min(x1) + start;
                let dash_x1 = x0.min(x1) + end;
                dash_vertices.extend(create_rect_vertices(
                    &Rect::new(dash_x0, y0, dash_x1 - dash_x0, thickness),
                    color,
                ));
            } else {
                let dash_y0 = y0.min(y1) + start;
                let dash_y1 = y0.min(y1) + end;
                dash_vertices.extend(create_rect_vertices(
                    &Rect::new(x0, dash_y0, thickness, dash_y1 - dash_y0),
                    color,
                ));
            }
        }
        dash_vertices
    };

    // Top edge (horizontal)
    vertices.extend(create_dashes(rect.x, rect.y, rect.right(), rect.y, true));

    // Bottom edge (horizontal)
    vertices.extend(create_dashes(rect.x, rect.bottom() - thickness, rect.right(), rect.bottom() - thickness, true));

    // Left edge (vertical)
    vertices.extend(create_dashes(rect.x, rect.y + thickness, rect.x, rect.bottom() - thickness, false));

    // Right edge (vertical)
    vertices.extend(create_dashes(rect.right() - thickness, rect.y + thickness, rect.right() - thickness, rect.bottom() - thickness, false));

    vertices
}

/// Theme colors - Classic dark mode
pub mod theme {
    use crate::ui::Color;

    // Classic dark theme - neutral grays, no blue tint
    pub const BACKGROUND: Color = Color::rgba(0.051, 0.051, 0.051, 1.0);      // #0d0d0d - near black
    pub const SURFACE: Color = Color::rgba(0.102, 0.102, 0.102, 1.0);         // #1a1a1a - panels
    pub const SURFACE_RAISED: Color = Color::rgba(0.145, 0.145, 0.145, 1.0);  // #252525 - elevated elements
    pub const SURFACE_HOVER: Color = Color::rgba(0.180, 0.180, 0.180, 1.0);   // #2e2e2e - hover states
    pub const BORDER: Color = Color::rgba(0.200, 0.200, 0.200, 1.0);          // #333333 - subtle borders
    pub const BORDER_LIGHT: Color = Color::rgba(0.250, 0.250, 0.250, 1.0);    // #404040 - emphasized borders
    pub const TEXT: Color = Color::rgba(0.878, 0.878, 0.878, 1.0);            // #e0e0e0 - primary text
    pub const TEXT_BRIGHT: Color = Color::rgba(0.940, 0.940, 0.940, 1.0);     // #f0f0f0 - emphasized text
    pub const TEXT_MUTED: Color = Color::rgba(0.502, 0.502, 0.502, 1.0);      // #808080 - secondary text

    // Status colors - slightly desaturated for dark theme
    pub const STATUS_CLEAN: Color = Color::rgba(0.298, 0.686, 0.314, 1.0);    // #4CAF50 (Green)
    pub const STATUS_BEHIND: Color = Color::rgba(1.000, 0.596, 0.000, 1.0);   // #FF9800 (Orange)
    pub const STATUS_DIRTY: Color = Color::rgba(0.937, 0.325, 0.314, 1.0);    // #EF5350 (Red)
    pub const STATUS_AHEAD: Color = Color::rgba(0.259, 0.647, 0.961, 1.0);    // #42A5F5 (Blue)

    // Branch colors - vibrant but balanced
    pub const BRANCH_PRIMARY: Color = Color::rgba(0.259, 0.647, 0.961, 1.0);  // #42A5F5 (Blue)
    pub const BRANCH_FEATURE: Color = Color::rgba(0.298, 0.686, 0.314, 1.0);  // #4CAF50 (Green)
    pub const BRANCH_RELEASE: Color = Color::rgba(1.000, 0.596, 0.000, 1.0);  // #FF9800 (Orange)
    pub const BRANCH_HOTFIX: Color = Color::rgba(0.671, 0.396, 0.859, 1.0);   // #AB65DB (Purple)
    pub const BRANCH_REMOTE: Color = Color::rgba(0.620, 0.620, 0.620, 1.0);   // #9e9e9e (Gray)

    // Accent color for selections and focus
    pub const ACCENT: Color = Color::rgba(0.259, 0.647, 0.961, 1.0);          // #42A5F5 (Blue)
    pub const ACCENT_MUTED: Color = Color::rgba(0.259, 0.647, 0.961, 0.3);    // Blue at 30% opacity

    // Panel depth - slightly different backgrounds for visual hierarchy
    pub const PANEL_SIDEBAR: Color = Color::rgba(0.075, 0.075, 0.075, 1.0);   // #131313 - sidebar (darker)
    pub const PANEL_GRAPH: Color = Color::rgba(0.051, 0.051, 0.051, 1.0);     // #0d0d0d - graph (matches bg)
    pub const PANEL_STAGING: Color = Color::rgba(0.065, 0.065, 0.065, 1.0);   // #101010 - staging (slightly lighter)

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
