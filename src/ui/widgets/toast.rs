//! Toast notification overlay - auto-dismissing messages for operation feedback

use std::time::Instant;

use crate::ui::{Rect, TextRenderer};
use crate::ui::widget::{WidgetOutput, create_rounded_rect_vertices, create_rect_outline_vertices};

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
    /// How many seconds a toast lives
    const LIFETIME_SECS: f32 = 4.0;
    /// Fade starts this many seconds before death
    const FADE_DURATION: f32 = 1.0;

    fn age(&self, now: Instant) -> f32 {
        now.duration_since(self.created_at).as_secs_f32()
    }

    fn is_expired(&self, now: Instant) -> bool {
        self.age(now) >= Self::LIFETIME_SECS
    }

    /// 1.0 = fully opaque, 0.0 = gone
    fn opacity(&self, now: Instant) -> f32 {
        let age = self.age(now);
        let fade_start = Self::LIFETIME_SECS - Self::FADE_DURATION;
        if age < fade_start {
            1.0
        } else {
            ((Self::LIFETIME_SECS - age) / Self::FADE_DURATION).clamp(0.0, 1.0)
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

    fn icon_prefix(&self) -> &'static str {
        match self.severity {
            ToastSeverity::Success => "[ok] ",
            ToastSeverity::Error   => "[!!] ",
            ToastSeverity::Info    => "[i]  ",
        }
    }
}

/// Manages a stack of toast notifications
pub struct ToastManager {
    toasts: Vec<Toast>,
}

impl ToastManager {
    pub fn new() -> Self {
        Self { toasts: Vec::new() }
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

    /// Render toasts at the bottom-center of `screen_bounds`.
    pub fn layout(&self, text_renderer: &TextRenderer, screen_bounds: Rect) -> WidgetOutput {
        let now = Instant::now();
        let mut output = WidgetOutput::new();

        if self.toasts.is_empty() {
            return output;
        }

        let line_height = text_renderer.line_height();
        let toast_width = 400.0_f32.min(screen_bounds.width - 40.0);
        let toast_height = line_height + 16.0; // padding top + bottom
        let spacing = 6.0;
        let bottom_margin = 24.0;

        // Stack from bottom up: newest toast at the bottom
        let count = self.toasts.len();
        for (i, toast) in self.toasts.iter().enumerate() {
            let opacity = toast.opacity(now);
            if opacity <= 0.0 {
                continue;
            }

            // Position: bottom-center, with older toasts above newer ones
            let slot = (count - 1 - i) as f32; // 0 = newest (bottom)
            let toast_x = screen_bounds.x + (screen_bounds.width - toast_width) / 2.0;
            let toast_y = screen_bounds.bottom()
                - bottom_margin
                - (slot + 1.0) * toast_height
                - slot * spacing;

            let rect = Rect::new(toast_x, toast_y, toast_width, toast_height);

            // Background
            output.spline_vertices.extend(create_rounded_rect_vertices(&rect, toast.bg_color(opacity), 6.0));

            // Border
            output.spline_vertices.extend(create_rect_outline_vertices(
                &rect,
                toast.border_color(opacity),
                1.0,
            ));

            // Text: icon prefix + message
            let display = format!("{}{}", toast.icon_prefix(), toast.message);
            let text_x = rect.x + 10.0;
            let text_y = rect.y + (rect.height - line_height) / 2.0;
            output.text_vertices.extend(text_renderer.layout_text(
                &display,
                text_x,
                text_y,
                toast.text_color(opacity),
            ));
        }

        output
    }
}
