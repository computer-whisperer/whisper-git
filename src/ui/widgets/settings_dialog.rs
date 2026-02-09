//! Settings dialog - modal overlay for configuring application preferences

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{
    create_rect_vertices, create_rect_outline_vertices, theme, Widget, WidgetId, WidgetOutput, WidgetState,
};
use crate::ui::widgets::Button;
use crate::ui::{Rect, TextRenderer};

/// Actions from the settings dialog
#[derive(Clone, Debug)]
pub enum SettingsDialogAction {
    Close,
}

/// A modal dialog for configuring application settings
#[allow(dead_code)]
pub struct SettingsDialog {
    id: WidgetId,
    state: WidgetState,
    visible: bool,
    close_button: Button,
    pending_action: Option<SettingsDialogAction>,
    // Settings state:
    pub show_avatars: bool,
    pub scroll_speed: f32, // 1.0 = normal, 2.0 = fast
}

impl SettingsDialog {
    pub fn new() -> Self {
        Self {
            id: WidgetId::new(),
            state: WidgetState::new(),
            visible: false,
            close_button: Button::new("Close"),
            pending_action: None,
            show_avatars: true,
            scroll_speed: 1.0,
        }
    }

    pub fn show(&mut self) {
        self.visible = true;
    }

    pub fn hide(&mut self) {
        self.visible = false;
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn take_action(&mut self) -> Option<SettingsDialogAction> {
        self.pending_action.take()
    }

    /// Compute dialog bounds centered in screen
    fn dialog_bounds(&self, screen: Rect, scale: f32) -> Rect {
        let dialog_w = (450.0 * scale).min(screen.width * 0.8);
        let dialog_h = (300.0 * scale).min(screen.height * 0.6);
        let dialog_x = screen.x + (screen.width - dialog_w) / 2.0;
        let dialog_y = screen.y + (screen.height - dialog_h) / 2.0;
        Rect::new(dialog_x, dialog_y, dialog_w, dialog_h)
    }

    /// Compute toggle option bounds for a setting row
    fn toggle_bounds(
        &self,
        dialog: &Rect,
        row_y: f32,
        row_h: f32,
        scale: f32,
    ) -> (Rect, Rect) {
        let padding = 16.0 * scale;
        let option_w = 70.0 * scale;
        let gap = 8.0 * scale;
        let right_edge = dialog.right() - padding;
        let off_rect = Rect::new(right_edge - option_w, row_y, option_w, row_h);
        let on_rect = Rect::new(right_edge - option_w * 2.0 - gap, row_y, option_w, row_h);
        (on_rect, off_rect)
    }
}

impl Widget for SettingsDialog {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        if !self.visible {
            return EventResponse::Ignored;
        }

        let scale = (bounds.height / 720.0).max(1.0);
        let dialog = self.dialog_bounds(bounds, scale);
        let padding = 16.0 * scale;
        let line_h = 32.0 * scale;
        let title_h = 40.0 * scale;

        // Row positions
        let row1_y = dialog.y + title_h + padding;
        let row2_y = row1_y + line_h + 12.0 * scale;

        // Toggle bounds
        let (av_on, av_off) = self.toggle_bounds(&dialog, row1_y, line_h, scale);
        let (sp_normal, sp_fast) = self.toggle_bounds(&dialog, row2_y, line_h, scale);

        // Close button bounds
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let close_x = dialog.right() - padding - button_w;
        let close_bounds = Rect::new(close_x, button_y, button_w, line_h);

        // Handle keyboard shortcuts
        if let InputEvent::KeyDown { key, .. } = event
            && *key == Key::Escape
        {
            self.pending_action = Some(SettingsDialogAction::Close);
            self.hide();
            return EventResponse::Consumed;
        }

        // Handle clicks on toggle options
        if let InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } = event {
            // Avatar toggles
            if av_on.contains(*x, *y) {
                self.show_avatars = true;
                return EventResponse::Consumed;
            }
            if av_off.contains(*x, *y) {
                self.show_avatars = false;
                return EventResponse::Consumed;
            }
            // Scroll speed toggles
            if sp_normal.contains(*x, *y) {
                self.scroll_speed = 1.0;
                return EventResponse::Consumed;
            }
            if sp_fast.contains(*x, *y) {
                self.scroll_speed = 2.0;
                return EventResponse::Consumed;
            }
        }

        // Route to close button
        if self.close_button.handle_event(event, close_bounds).is_consumed() {
            if self.close_button.was_clicked() {
                self.pending_action = Some(SettingsDialogAction::Close);
                self.hide();
            }
            return EventResponse::Consumed;
        }

        // Click outside dialog dismisses
        if let InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } = event
            && !dialog.contains(*x, *y)
        {
            self.pending_action = Some(SettingsDialogAction::Close);
            self.hide();
            return EventResponse::Consumed;
        }

        // Consume all events while dialog is visible (modal)
        EventResponse::Consumed
    }

    fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        if !self.visible {
            return output;
        }

        let scale = (bounds.height / 720.0).max(1.0);
        let dialog = self.dialog_bounds(bounds, scale);
        let padding = 16.0 * scale;
        let line_h = 32.0 * scale;
        let title_h = 40.0 * scale;
        let line_height = text_renderer.line_height();

        // Semi-transparent backdrop
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            [0.0, 0.0, 0.0, 0.8],
        ));

        // Drop shadow
        let shadow_offset = 3.0 * scale;
        let shadow_rect = Rect::new(
            dialog.x + shadow_offset,
            dialog.y + shadow_offset,
            dialog.width,
            dialog.height,
        );
        output.spline_vertices.extend(create_rect_vertices(
            &shadow_rect,
            [0.0, 0.0, 0.0, 0.5],
        ));

        // Dialog background
        output.spline_vertices.extend(create_rect_vertices(
            &dialog,
            theme::SURFACE_RAISED.lighten(0.06).to_array(),
        ));

        // Dialog border (2px)
        output.spline_vertices.extend(create_rect_outline_vertices(
            &dialog,
            theme::BORDER_LIGHT.lighten(0.05).to_array(),
            2.0,
        ));

        // Title
        let title_y = dialog.y + padding;
        output.text_vertices.extend(text_renderer.layout_text(
            "Settings",
            dialog.x + padding,
            title_y,
            theme::TEXT_BRIGHT.to_array(),
        ));

        // --- Setting rows ---
        let row1_y = dialog.y + title_h + padding;
        let row2_y = row1_y + line_h + 12.0 * scale;

        // Row 1: Show Avatars
        let label_y = row1_y + (line_h - line_height) / 2.0;
        output.text_vertices.extend(text_renderer.layout_text(
            "Show Avatars",
            dialog.x + padding,
            label_y,
            theme::TEXT.to_array(),
        ));

        let (av_on, av_off) = self.toggle_bounds(&dialog, row1_y, line_h, scale);
        self.render_toggle_option(&mut output, text_renderer, &av_on, "ON", self.show_avatars);
        self.render_toggle_option(&mut output, text_renderer, &av_off, "OFF", !self.show_avatars);

        // Row 2: Scroll Speed
        let label_y2 = row2_y + (line_h - line_height) / 2.0;
        output.text_vertices.extend(text_renderer.layout_text(
            "Scroll Speed",
            dialog.x + padding,
            label_y2,
            theme::TEXT.to_array(),
        ));

        let (sp_normal, sp_fast) = self.toggle_bounds(&dialog, row2_y, line_h, scale);
        self.render_toggle_option(&mut output, text_renderer, &sp_normal, "Normal", self.scroll_speed < 1.5);
        self.render_toggle_option(&mut output, text_renderer, &sp_fast, "Fast", self.scroll_speed >= 1.5);

        // Close button at bottom
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let close_x = dialog.right() - padding - button_w;
        let close_bounds = Rect::new(close_x, button_y, button_w, line_h);
        output.extend(self.close_button.layout(text_renderer, close_bounds));

        output
    }
}

impl SettingsDialog {
    /// Render a single toggle option button (active or inactive)
    fn render_toggle_option(
        &self,
        output: &mut WidgetOutput,
        text_renderer: &TextRenderer,
        rect: &Rect,
        label: &str,
        is_active: bool,
    ) {
        let line_height = text_renderer.line_height();

        // Background
        let bg_color = if is_active {
            theme::ACCENT.with_alpha(0.3).to_array()
        } else {
            theme::SURFACE_RAISED.to_array()
        };
        output.spline_vertices.extend(create_rect_vertices(rect, bg_color));

        // Border
        let border_color = if is_active {
            theme::ACCENT.to_array()
        } else {
            theme::BORDER.to_array()
        };
        output.spline_vertices.extend(create_rect_outline_vertices(rect, border_color, 1.0));

        // Label text (centered)
        let text_width = text_renderer.measure_text(label);
        let text_x = rect.x + (rect.width - text_width) / 2.0;
        let text_y = rect.y + (rect.height - line_height) / 2.0;
        let text_color = if is_active {
            theme::TEXT_BRIGHT.to_array()
        } else {
            theme::TEXT_MUTED.to_array()
        };
        output.text_vertices.extend(text_renderer.layout_text(
            label,
            text_x,
            text_y,
            text_color,
        ));
    }
}
