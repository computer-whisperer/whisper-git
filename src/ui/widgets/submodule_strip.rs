//! Compact pill bar showing submodule status at the bottom of the commit graph area

use crate::git::SubmoduleInfo;
use crate::input::{EventResponse, InputEvent, MouseButton};
use crate::ui::{Color, Rect, TextRenderer};
use crate::ui::widget::{
    WidgetOutput, create_rect_vertices, create_rounded_rect_vertices,
    create_rounded_rect_outline_vertices, theme,
};

/// Actions emitted by the submodule strip
pub enum SubmoduleStripAction {
    /// Right-click on a pill: open context menu with (name, x, y)
    OpenContextMenu(String, f32, f32),
}

/// Horizontal strip of submodule status pills
pub struct SubmoduleStatusStrip {
    pub submodules: Vec<SubmoduleInfo>,
    /// Pill bounds for hit testing: (rect, submodule_name)
    pill_bounds: Vec<(Rect, String)>,
    /// Pending action to be consumed by the app
    pending_action: Option<SubmoduleStripAction>,
}

impl SubmoduleStatusStrip {
    pub fn new() -> Self {
        Self {
            submodules: Vec::new(),
            pill_bounds: Vec::new(),
            pending_action: None,
        }
    }

    /// Take the pending action, if any
    pub fn take_action(&mut self) -> Option<SubmoduleStripAction> {
        self.pending_action.take()
    }

    /// The height of the strip in pixels (scale-aware)
    pub fn height(scale: f32) -> f32 {
        28.0 * scale
    }

    /// Determine the status color for a submodule
    fn status_color(sm: &SubmoduleInfo) -> Color {
        if sm.is_dirty {
            // Yellow - dirty working tree
            Color::rgba(1.0, 0.718, 0.302, 1.0) // #FFB74D
        } else if sm.branch == "detached" || (sm.head_oid.is_none() && sm.workdir_oid.is_none()) {
            // Red - detached or missing OIDs
            Color::rgba(0.937, 0.325, 0.314, 1.0) // #EF5350
        } else if sm.workdir_oid != sm.head_oid && sm.head_oid.is_some() && sm.workdir_oid.is_some() {
            // Blue - staged pointer change (workdir differs from pinned)
            Color::rgba(0.149, 0.776, 0.855, 1.0) // #26C6DA
        } else {
            // Green - clean
            Color::rgba(0.400, 0.733, 0.416, 1.0) // #66BB6A
        }
    }

    /// Handle input events for the strip
    pub fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        match event {
            InputEvent::MouseDown { button: MouseButton::Right, x, y, .. } => {
                if bounds.contains(*x, *y) {
                    // Check if right-click is on a pill
                    for (pill_rect, name) in &self.pill_bounds {
                        if pill_rect.contains(*x, *y) {
                            self.pending_action = Some(SubmoduleStripAction::OpenContextMenu(
                                name.clone(), *x, *y,
                            ));
                            return EventResponse::Consumed;
                        }
                    }
                }
                EventResponse::Ignored
            }
            _ => EventResponse::Ignored,
        }
    }

    /// Render the strip into the given bounds
    pub fn layout(&mut self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        self.pill_bounds.clear();

        if self.submodules.is_empty() {
            return WidgetOutput::new();
        }

        let mut output = WidgetOutput::new();

        // Background fill
        let bg_color = theme::SURFACE.to_array();
        output.spline_vertices.extend(create_rect_vertices(&bounds, bg_color));

        // Top border
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(bounds.x, bounds.y, bounds.width, 1.0),
            theme::BORDER.to_array(),
        ));

        let small_lh = text_renderer.line_height_small();
        let text_y = bounds.y + (bounds.height - small_lh) / 2.0;
        let mut x = bounds.x + 10.0;
        let right_edge = bounds.right() - 6.0;

        // Section label
        let label = "SUBMODULES";
        let label_w = text_renderer.measure_text_scaled(label, 0.85);
        output.text_vertices.extend(
            text_renderer.layout_text_small(label, x, text_y, theme::TEXT_MUTED.to_array()),
        );
        x += label_w + 10.0;

        // Pill metrics
        let pill_radius = 4.0;
        let pill_pad_h = 6.0;
        let pill_pad_v = 3.0;
        let gap = 6.0;
        let dot = "\u{25CF} "; // ● + space

        for sm in &self.submodules {
            let color = Self::status_color(sm);
            let color_arr = color.to_array();

            // Build pill text: "● name" + optional "+N"
            let dot_w = text_renderer.measure_text_scaled(dot, 0.85);
            let name_w = text_renderer.measure_text_scaled(&sm.name, 0.85);

            let suffix = if sm.ahead > 0 {
                format!(" +{}", sm.ahead)
            } else {
                String::new()
            };
            let suffix_w = if !suffix.is_empty() {
                text_renderer.measure_text_scaled(&suffix, 0.85)
            } else {
                0.0
            };

            let content_w = dot_w + name_w + suffix_w;
            let pill_w = content_w + pill_pad_h * 2.0;

            // Clip: stop if pill won't fit
            if x + pill_w > right_edge {
                break;
            }

            let pill_rect = Rect::new(
                x,
                text_y - pill_pad_v,
                pill_w,
                small_lh + pill_pad_v * 2.0,
            );

            // Store pill bounds for hit testing
            self.pill_bounds.push((pill_rect, sm.name.clone()));

            // Pill background (status color at 0.15 alpha)
            let bg = color.with_alpha(0.15).to_array();
            output.spline_vertices.extend(create_rounded_rect_vertices(&pill_rect, bg, pill_radius));

            // Pill outline (status color at 0.35 alpha)
            let outline = color.with_alpha(0.35).to_array();
            output.spline_vertices.extend(create_rounded_rect_outline_vertices(
                &pill_rect, outline, pill_radius, 1.0,
            ));

            // Status dot (in status color)
            let text_x = x + pill_pad_h;
            output.text_vertices.extend(
                text_renderer.layout_text_small(dot, text_x, text_y, color_arr),
            );

            // Name text
            output.text_vertices.extend(
                text_renderer.layout_text_small(&sm.name, text_x + dot_w, text_y, theme::TEXT_BRIGHT.to_array()),
            );

            // Ahead suffix
            if !suffix.is_empty() {
                output.text_vertices.extend(
                    text_renderer.layout_text_small(&suffix, text_x + dot_w + name_w, text_y, color_arr),
                );
            }

            x += pill_w + gap;
        }

        output
    }
}
