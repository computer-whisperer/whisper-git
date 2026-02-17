//! Tooltip widget - hover popup showing full text for truncated content

use std::time::Instant;

use crate::ui::{Rect, TextRenderer};
use crate::ui::text_util::wrap_text;
use crate::ui::widget::{WidgetOutput, create_rounded_rect_vertices, create_rounded_rect_outline_vertices, theme};

/// A tooltip that appears after hovering over a truncated element.
///
/// Usage each frame:
/// 1. Call `begin_frame()` before processing hover
/// 2. Call `offer()` for each element that wants a tooltip (if hovered)
/// 3. Call `end_frame()` after all offers
/// 4. Call `update()` to check the delay timer
/// 5. Call `layout()` to render the tooltip if visible
pub struct Tooltip {
    /// Bounds of the element currently being hovered (for identity tracking)
    hover_bounds: Option<Rect>,
    /// Full text to display
    text: String,
    /// When hovering started (for delay)
    hover_start: Option<Instant>,
    /// Mouse position when offered
    mouse_x: f32,
    mouse_y: f32,
    /// Whether the tooltip should be rendered
    visible: bool,
    /// Whether any element offered a tooltip this frame
    offered_this_frame: bool,
}

impl Tooltip {
    /// Hover delay before tooltip appears (milliseconds)
    const DELAY_MS: u128 = 400;
    /// Maximum tooltip width in logical pixels (before scale)
    const MAX_WIDTH: f32 = 400.0;
    /// Corner radius
    const CORNER_RADIUS: f32 = 4.0;
    /// Padding inside tooltip
    const PADDING: f32 = 6.0;
    /// Maximum number of lines before truncating
    const MAX_LINES: usize = 16;

    pub fn new() -> Self {
        Self {
            hover_bounds: None,
            text: String::new(),
            hover_start: None,
            mouse_x: 0.0,
            mouse_y: 0.0,
            visible: false,
            offered_this_frame: false,
        }
    }

    /// Call at the start of each hover processing pass.
    pub fn begin_frame(&mut self) {
        self.offered_this_frame = false;
    }

    /// Offer a tooltip for an element. Call this when the mouse is over a truncated element.
    /// `bounds` identifies the element (used to detect when hovering moves to a different element).
    /// `full_text` is the complete text to show in the tooltip.
    pub fn offer(&mut self, bounds: Rect, full_text: &str, mouse_x: f32, mouse_y: f32) {
        self.offered_this_frame = true;
        self.mouse_x = mouse_x;
        self.mouse_y = mouse_y;

        // Check if this is the same element we were already hovering
        if let Some(prev) = self.hover_bounds {
            if Self::same_bounds(prev, bounds) {
                // Same element — keep the timer running
                return;
            }
        }

        // New element — reset timer
        self.hover_bounds = Some(bounds);
        self.text = full_text.to_string();
        self.hover_start = Some(Instant::now());
        self.visible = false;
    }

    /// Call at the end of each hover processing pass.
    /// Clears tooltip state if nothing was offered this frame.
    pub fn end_frame(&mut self) {
        if !self.offered_this_frame {
            self.hover_bounds = None;
            self.text.clear();
            self.hover_start = None;
            self.visible = false;
        }
    }

    /// Check the delay timer and make the tooltip visible when elapsed.
    pub fn update(&mut self) {
        if let Some(start) = self.hover_start {
            if !self.visible && start.elapsed().as_millis() >= Self::DELAY_MS {
                self.visible = true;
            }
        }
    }

    /// Render the tooltip if visible.
    pub fn layout(&self, text_renderer: &TextRenderer, screen_bounds: Rect, scale: f32) -> WidgetOutput {
        let mut output = WidgetOutput::new();
        if !self.visible || self.text.is_empty() {
            return output;
        }

        let padding = Self::PADDING * scale;
        let corner_radius = Self::CORNER_RADIUS * scale;
        let max_width = Self::MAX_WIDTH * scale;
        let line_height = text_renderer.line_height();

        // Word-wrap the text, capped to avoid giant tooltips
        let max_text_width = max_width - padding * 2.0;
        let mut lines = wrap_text(&self.text, max_text_width, text_renderer);
        if lines.len() > Self::MAX_LINES {
            lines.truncate(Self::MAX_LINES);
            lines.push("...".to_string());
        }
        let num_lines = lines.len().max(1);

        // Measure actual content width
        let content_width = lines.iter()
            .map(|l| text_renderer.measure_text(l))
            .fold(0.0_f32, f32::max);

        let tooltip_width = content_width + padding * 2.0;
        let tooltip_height = (num_lines as f32 * line_height) + padding * 2.0;

        // Position: below-right of cursor, clamped to screen
        let cursor_offset = 12.0 * scale;
        let mut tip_x = self.mouse_x + cursor_offset;
        let mut tip_y = self.mouse_y + cursor_offset;

        // Clamp to screen bounds
        if tip_x + tooltip_width > screen_bounds.right() {
            tip_x = (self.mouse_x - tooltip_width - cursor_offset / 2.0).max(screen_bounds.x);
        }
        if tip_y + tooltip_height > screen_bounds.bottom() {
            tip_y = (self.mouse_y - tooltip_height - cursor_offset / 2.0).max(screen_bounds.y);
        }

        let rect = Rect::new(tip_x, tip_y, tooltip_width, tooltip_height);

        // Background
        let bg_color = theme::SURFACE_RAISED.lighten(0.04).to_array();
        output.spline_vertices.extend(create_rounded_rect_vertices(&rect, bg_color, corner_radius));

        // Border
        output.spline_vertices.extend(create_rounded_rect_outline_vertices(
            &rect, theme::BORDER_LIGHT.to_array(), corner_radius, 1.0,
        ));

        // Text lines
        let text_x = rect.x + padding;
        for (i, line) in lines.iter().enumerate() {
            let text_y = rect.y + padding + i as f32 * line_height;
            output.text_vertices.extend(text_renderer.layout_text(
                line, text_x, text_y,
                theme::TEXT_BRIGHT.to_array(),
            ));
        }

        output
    }

    /// Check if two rects represent the same element (approximate equality)
    fn same_bounds(a: Rect, b: Rect) -> bool {
        (a.x - b.x).abs() < 0.5
            && (a.y - b.y).abs() < 0.5
            && (a.width - b.width).abs() < 0.5
            && (a.height - b.height).abs() < 0.5
    }
}
