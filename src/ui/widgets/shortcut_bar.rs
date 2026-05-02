//! Keyboard shortcut status bar - shows context-sensitive shortcuts for the focused panel

use crate::ui::widget::{
    WidgetOutput, create_rect_vertices, create_rounded_rect_outline_vertices,
    create_rounded_rect_vertices, theme,
};
use crate::ui::{Rect, TextRenderer};

/// Which panel shortcuts to display
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutContext {
    Graph,
    Staging,
    Sidebar,
}

/// A single shortcut hint: key label + action description.
/// `group_break` introduces a divider before this hint to visually separate
/// context-specific shortcuts from global git operations.
struct ShortcutHint {
    key: &'static str,
    action: &'static str,
    group_break: bool,
}

/// Thin bar rendered below the header showing keyboard shortcuts
pub struct ShortcutBar {
    context: ShortcutContext,
    /// Show "Ctrl+O New Tab" hint on the right (when only one tab is open)
    pub show_new_tab_hint: bool,
}

impl ShortcutBar {
    pub fn new() -> Self {
        Self {
            context: ShortcutContext::Graph,
            show_new_tab_hint: false,
        }
    }

    /// Update which panel's shortcuts to display
    pub fn set_context(&mut self, context: ShortcutContext) {
        self.context = context;
    }

    /// Render the shortcut bar into the given bounds
    pub fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        // Subtle background - slightly different from main panels
        let bg_color = theme::SURFACE.to_array();
        output
            .spline_vertices
            .extend(create_rect_vertices(&bounds, bg_color));

        // Bottom border
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(bounds.x, bounds.bottom() - 1.0, bounds.width, 1.0),
            theme::BORDER.to_array(),
        ));

        let hints = self.hints_for_context();
        let text_scale = 0.92;
        let scaled_lh = text_renderer.line_height() * text_scale;
        let text_y = bounds.y + (bounds.height - scaled_lh) / 2.0;
        let mut x = bounds.x + 12.0;

        let key_color = theme::TEXT_BRIGHT.to_array();
        let action_color = theme::TEXT.to_array();
        let pill_bg = theme::SURFACE_RAISED.to_array();
        let pill_outline = theme::BORDER_LIGHT.to_array();
        let pill_radius = 3.0;
        let pill_pad_h = 5.0;
        let pill_pad_v = 3.0;
        let char_w = text_renderer.measure_text_scaled(" ", text_scale);

        for (i, hint) in hints.iter().enumerate() {
            // Gap between hints
            if i > 0 {
                x += char_w * 2.0;
            }

            // Group divider — thin vertical line separating shortcut groups
            if hint.group_break {
                let div_x = x;
                let div_pad = 6.0;
                let div_h = scaled_lh + pill_pad_v * 2.0;
                let div_y = text_y - pill_pad_v;
                output.spline_vertices.extend(create_rect_vertices(
                    &Rect::new(div_x, div_y + 2.0, 1.0, div_h - 4.0),
                    theme::BORDER.to_array(),
                ));
                x += 1.0 + div_pad * 2.0;
            }

            // Key pill: rounded rect background behind key text
            let key_w = text_renderer.measure_text_scaled(hint.key, text_scale);
            let pill_rect = Rect::new(
                x - pill_pad_h,
                text_y - pill_pad_v,
                key_w + pill_pad_h * 2.0,
                scaled_lh + pill_pad_v * 2.0,
            );
            output.spline_vertices.extend(create_rounded_rect_vertices(
                &pill_rect,
                pill_bg,
                pill_radius,
            ));
            output
                .spline_vertices
                .extend(create_rounded_rect_outline_vertices(
                    &pill_rect,
                    pill_outline,
                    pill_radius,
                    1.0,
                ));

            // Key label text inside pill
            output.text_vertices.extend(
                text_renderer.layout_text_scaled(hint.key, x, text_y, key_color, text_scale),
            );
            x += key_w + pill_pad_h;

            // Space
            x += char_w;

            // Action description
            output
                .text_vertices
                .extend(text_renderer.layout_text_scaled(
                    hint.action,
                    x,
                    text_y,
                    action_color,
                    text_scale,
                ));
            x += text_renderer.measure_text_scaled(hint.action, text_scale);
        }

        // Right-aligned "Ctrl+O New Tab" hint when only one tab is open
        if self.show_new_tab_hint {
            let hint_key = "Ctrl+O";
            let hint_action = "New Tab";
            let key_w = text_renderer.measure_text_scaled(hint_key, text_scale);
            let action_w = text_renderer.measure_text_scaled(hint_action, text_scale);
            let total_width = key_w + pill_pad_h + char_w + action_w;
            let hint_x = bounds.right() - total_width - 12.0;

            // Pill behind key
            let pill_rect = Rect::new(
                hint_x - pill_pad_h,
                text_y - pill_pad_v,
                key_w + pill_pad_h * 2.0,
                scaled_lh + pill_pad_v * 2.0,
            );
            output.spline_vertices.extend(create_rounded_rect_vertices(
                &pill_rect,
                pill_bg,
                pill_radius,
            ));
            output
                .spline_vertices
                .extend(create_rounded_rect_outline_vertices(
                    &pill_rect,
                    pill_outline,
                    pill_radius,
                    1.0,
                ));

            output
                .text_vertices
                .extend(text_renderer.layout_text_scaled(
                    hint_key,
                    hint_x,
                    text_y,
                    theme::TEXT.to_array(),
                    text_scale,
                ));
            let action_x = hint_x + key_w + pill_pad_h + char_w;
            output
                .text_vertices
                .extend(text_renderer.layout_text_scaled(
                    hint_action,
                    action_x,
                    text_y,
                    theme::TEXT_MUTED.to_array(),
                    text_scale,
                ));
        }

        output
    }

    fn hints_for_context(&self) -> Vec<ShortcutHint> {
        let mut hints = match self.context {
            ShortcutContext::Graph => vec![
                ShortcutHint {
                    key: "j/k",
                    action: "Navigate",
                    group_break: false,
                },
                ShortcutHint {
                    key: "Enter",
                    action: "Select",
                    group_break: false,
                },
                ShortcutHint {
                    key: "Ctrl+F",
                    action: "Search",
                    group_break: false,
                },
                ShortcutHint {
                    key: "/",
                    action: "Filter",
                    group_break: false,
                },
                ShortcutHint {
                    key: "Tab",
                    action: "Next Panel",
                    group_break: false,
                },
            ],
            ShortcutContext::Staging => vec![
                ShortcutHint {
                    key: "Tab",
                    action: "Cycle Fields",
                    group_break: false,
                },
                ShortcutHint {
                    key: "Ctrl+Enter",
                    action: "Commit",
                    group_break: false,
                },
                ShortcutHint {
                    key: "Tab",
                    action: "Next Panel",
                    group_break: false,
                },
            ],
            ShortcutContext::Sidebar => vec![
                ShortcutHint {
                    key: "j/k",
                    action: "Navigate",
                    group_break: false,
                },
                ShortcutHint {
                    key: "Enter",
                    action: "Checkout",
                    group_break: false,
                },
                ShortcutHint {
                    key: "d",
                    action: "Delete",
                    group_break: false,
                },
                ShortcutHint {
                    key: "Tab",
                    action: "Next Panel",
                    group_break: false,
                },
            ],
        };
        // Global git operation shortcuts shown in all contexts.
        // First entry gets a group divider to visually separate from context shortcuts.
        hints.push(ShortcutHint {
            key: "Ctrl+Shift+F",
            action: "Fetch",
            group_break: true,
        });
        hints.push(ShortcutHint {
            key: "Ctrl+Shift+L",
            action: "Pull",
            group_break: false,
        });
        hints.push(ShortcutHint {
            key: "Ctrl+Shift+P",
            action: "Push",
            group_break: false,
        });
        hints.push(ShortcutHint {
            key: "`",
            action: "Terminal",
            group_break: false,
        });
        hints
    }
}
