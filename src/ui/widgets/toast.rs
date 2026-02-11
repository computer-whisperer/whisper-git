//! Toast notification overlay - auto-dismissing messages for operation feedback

use std::time::Instant;

use crate::input::InputEvent;
use crate::ui::{Rect, TextRenderer};
use crate::ui::widget::{WidgetOutput, create_rounded_rect_vertices, create_rounded_rect_outline_vertices};

/// Severity determines the color scheme
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastSeverity {
    Success,
    Error,
    Info,
}

/// A single toast notification
struct Toast {
    message: String,
    severity: ToastSeverity,
    created_at: Instant,
}

impl Toast {
    /// Fade starts this many seconds before death
    const FADE_DURATION: f32 = 1.0;

    /// Severity-specific lifetime in seconds
    fn lifetime(&self) -> f32 {
        match self.severity {
            ToastSeverity::Success => 3.0,
            ToastSeverity::Info => 5.0,
            ToastSeverity::Error => 8.0,
        }
    }

    fn age(&self, now: Instant) -> f32 {
        now.duration_since(self.created_at).as_secs_f32()
    }

    fn is_expired(&self, now: Instant) -> bool {
        self.age(now) >= self.lifetime()
    }

    /// 1.0 = fully opaque, 0.0 = gone
    fn opacity(&self, now: Instant) -> f32 {
        let age = self.age(now);
        let lifetime = self.lifetime();
        let fade_start = lifetime - Self::FADE_DURATION;
        if age < fade_start {
            1.0
        } else {
            ((lifetime - age) / Self::FADE_DURATION).clamp(0.0, 1.0)
        }
    }

    fn bg_color(&self, opacity: f32) -> [f32; 4] {
        match self.severity {
            ToastSeverity::Success => [0.102, 0.227, 0.102, 0.95 * opacity],
            ToastSeverity::Error   => [0.227, 0.102, 0.102, 0.95 * opacity],
            ToastSeverity::Info    => [0.102, 0.102, 0.227, 0.95 * opacity],
        }
    }

    fn border_color(&self, opacity: f32) -> [f32; 4] {
        match self.severity {
            ToastSeverity::Success => [0.298, 0.686, 0.314, opacity],
            ToastSeverity::Error   => [0.937, 0.325, 0.314, opacity],
            ToastSeverity::Info    => [0.259, 0.647, 0.961, opacity],
        }
    }

    fn text_color(&self, opacity: f32) -> [f32; 4] {
        [0.878, 0.878, 0.878, opacity]
    }
}

/// Word-wrap a message into lines that fit within max_width.
/// Returns a Vec of line strings.
fn wrap_text(message: &str, max_width: f32, text_renderer: &TextRenderer) -> Vec<String> {
    let mut lines = Vec::new();
    let words: Vec<&str> = message.split_whitespace().collect();
    if words.is_empty() {
        lines.push(String::new());
        return lines;
    }

    let space_width = text_renderer.measure_text(" ");
    let mut current_line = String::new();
    let mut current_width = 0.0_f32;

    for word in &words {
        let word_width = text_renderer.measure_text(word);

        if current_line.is_empty() {
            // First word on the line - always accept it even if it overflows
            current_line.push_str(word);
            current_width = word_width;
        } else if current_width + space_width + word_width <= max_width {
            // Fits on current line
            current_line.push(' ');
            current_line.push_str(word);
            current_width += space_width + word_width;
        } else {
            // Doesn't fit - start new line
            lines.push(current_line);
            current_line = word.to_string();
            current_width = word_width;
        }
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    lines
}

/// Manages a stack of toast notifications
pub struct ToastManager {
    toasts: Vec<Toast>,
    /// Cached toast bounds from last layout, for click-to-dismiss hit testing.
    /// Each entry is (toast_index, rect).
    toast_bounds: Vec<(usize, Rect)>,
}

impl ToastManager {
    pub fn new() -> Self {
        Self {
            toasts: Vec::new(),
            toast_bounds: Vec::new(),
        }
    }

    /// Push a new toast. Evicts the oldest if we already have 3.
    pub fn push(&mut self, message: impl Into<String>, severity: ToastSeverity) {
        if self.toasts.len() >= 3 {
            self.toasts.remove(0);
        }
        self.toasts.push(Toast {
            message: message.into(),
            severity,
            created_at: Instant::now(),
        });
    }

    /// Remove expired toasts. Call each frame.
    pub fn update(&mut self, now: Instant) {
        self.toasts.retain(|t| !t.is_expired(now));
    }

    /// Handle input events - returns true if the event was consumed (click on a toast).
    pub fn handle_event(&mut self, event: &InputEvent, _screen_bounds: Rect) -> bool {
        match event {
            InputEvent::MouseDown { x, y, .. } => {
                // Check if click is on any toast (iterate bounds in reverse for topmost first)
                for &(toast_idx, ref rect) in self.toast_bounds.iter().rev() {
                    if rect.contains(*x, *y) && toast_idx < self.toasts.len() {
                        self.toasts.remove(toast_idx);
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Render toasts at the bottom-center of `screen_bounds`.
    pub fn layout(&mut self, text_renderer: &TextRenderer, screen_bounds: Rect, scale: f32) -> WidgetOutput {
        let now = Instant::now();
        let mut output = WidgetOutput::new();
        self.toast_bounds.clear();

        if self.toasts.is_empty() {
            return output;
        }

        let line_height = text_renderer.line_height();
        let corner_radius = 6.0 * scale;
        let border_thickness = 1.0 * scale;
        let spacing = 6.0 * scale;
        let bottom_margin = 24.0 * scale;
        let pad_top = 10.0 * scale;
        let pad_bottom = 10.0 * scale;
        let pad_left = 16.0 * scale;
        let pad_right = 16.0 * scale;
        let indicator_width = 4.0 * scale;
        let indicator_gap = 10.0 * scale; // gap between indicator bar and text
        let indicator_v_inset = 4.0 * scale; // vertical inset for the indicator bar

        let min_width = 200.0 * scale;
        let max_width = (screen_bounds.width * 0.6).max(min_width);
        let max_text_width = max_width - pad_left - indicator_width - indicator_gap - pad_right;

        // Pre-compute toast sizes (we need all heights to stack them)
        struct ToastLayout {
            lines: Vec<String>,
            toast_width: f32,
            toast_height: f32,
            opacity: f32,
        }
        let mut layouts: Vec<Option<ToastLayout>> = Vec::new();

        for toast in &self.toasts {
            let opacity = toast.opacity(now);
            if opacity <= 0.0 {
                layouts.push(None);
                continue;
            }

            let lines = wrap_text(&toast.message, max_text_width, text_renderer);
            let num_lines = lines.len().max(1);
            let text_width = lines.iter()
                .map(|l| text_renderer.measure_text(l))
                .fold(0.0_f32, f32::max);

            let content_width = indicator_width + indicator_gap + text_width;
            let toast_width = (content_width + pad_left + pad_right).clamp(min_width, max_width);
            let toast_height = pad_top + (num_lines as f32 * line_height) + pad_bottom;

            layouts.push(Some(ToastLayout {
                lines,
                toast_width,
                toast_height,
                opacity,
            }));
        }

        // Stack from bottom up: newest toast at the bottom
        let mut y_cursor = screen_bounds.bottom() - bottom_margin;
        let count = self.toasts.len();

        for i in (0..count).rev() {
            let Some(ref tl) = layouts[i] else { continue };

            let toast = &self.toasts[i];
            let opacity = tl.opacity;

            y_cursor -= tl.toast_height;
            let toast_x = screen_bounds.x + (screen_bounds.width - tl.toast_width) / 2.0;
            let rect = Rect::new(toast_x, y_cursor, tl.toast_width, tl.toast_height);

            // Store bounds for click-to-dismiss
            self.toast_bounds.push((i, rect));

            // Background
            output.spline_vertices.extend(create_rounded_rect_vertices(
                &rect, toast.bg_color(opacity), corner_radius,
            ));

            // Border (rounded, matching fill)
            output.spline_vertices.extend(create_rounded_rect_outline_vertices(
                &rect, toast.border_color(opacity), corner_radius, border_thickness,
            ));

            // Severity indicator bar (left side, full height minus padding)
            let bar_rect = Rect::new(
                rect.x + pad_left,
                rect.y + indicator_v_inset,
                indicator_width,
                rect.height - 2.0 * indicator_v_inset,
            );
            output.spline_vertices.extend(create_rounded_rect_vertices(
                &bar_rect, toast.border_color(opacity), indicator_width / 2.0,
            ));

            // Text lines
            let text_x = bar_rect.right() + indicator_gap;
            let text_start_y = rect.y + pad_top;
            for (line_idx, line) in tl.lines.iter().enumerate() {
                let text_y = text_start_y + line_idx as f32 * line_height;
                output.text_vertices.extend(text_renderer.layout_text(
                    line,
                    text_x,
                    text_y,
                    toast.text_color(opacity),
                ));
            }

            y_cursor -= spacing;
        }

        output
    }
}
