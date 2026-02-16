//! Settings dialog - modal overlay for configuring application preferences

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{
    create_dialog_backdrop, create_rect_vertices, create_rounded_rect_vertices, create_rect_outline_vertices, theme, Widget, WidgetOutput,
};
use crate::ui::widgets::Button;
use crate::ui::{Rect, TextRenderer};

/// Actions from the settings dialog
#[derive(Clone, Debug)]
pub enum SettingsDialogAction {
    Close,
}

/// A modal dialog for configuring application settings
pub struct SettingsDialog {
    visible: bool,
    close_button: Button,
    pending_action: Option<SettingsDialogAction>,
    // Settings state:
    pub show_avatars: bool,
    pub scroll_speed: f32, // 1.0 = normal, 2.0 = fast
    pub row_scale: f32,    // 1.0 = normal, 1.5 = large
    pub abbreviate_worktree_names: bool,
    pub time_spacing_strength: f32, // 0.3 = low, 1.0 = normal, 2.0 = high
    pub show_orphaned_commits: bool,
}

impl SettingsDialog {
    pub fn new() -> Self {
        Self {
            visible: false,
            close_button: Button::new("Close"),
            pending_action: None,
            show_avatars: true,
            scroll_speed: 1.0,
            row_scale: 1.0,
            abbreviate_worktree_names: true,
            time_spacing_strength: 1.0,
            show_orphaned_commits: true,
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
        let dialog_h = (545.0 * scale).min(screen.height * 0.7);
        let dialog_x = screen.x + (screen.width - dialog_w) / 2.0;
        let dialog_y = screen.y + (screen.height - dialog_h) / 2.0;
        Rect::new(dialog_x, dialog_y, dialog_w, dialog_h)
    }

    /// Compute toggle option bounds for a setting row (2 options)
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

    /// Compute triple toggle option bounds for a setting row (3 options)
    fn triple_toggle_bounds(
        &self,
        dialog: &Rect,
        row_y: f32,
        row_h: f32,
        scale: f32,
    ) -> (Rect, Rect, Rect) {
        let padding = 16.0 * scale;
        let option_w = 70.0 * scale;
        let gap = 8.0 * scale;
        let right_edge = dialog.right() - padding;
        let r3 = Rect::new(right_edge - option_w, row_y, option_w, row_h);
        let r2 = Rect::new(right_edge - option_w * 2.0 - gap, row_y, option_w, row_h);
        let r1 = Rect::new(right_edge - option_w * 3.0 - gap * 2.0, row_y, option_w, row_h);
        (r1, r2, r3)
    }
}

impl Widget for SettingsDialog {
    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        if !self.visible {
            return EventResponse::Ignored;
        }

        let scale = (bounds.height / 720.0).max(1.0);
        let dialog = self.dialog_bounds(bounds, scale);
        let padding = 16.0 * scale;
        let line_h = 32.0 * scale;
        let title_h = 40.0 * scale;
        let row_gap = 12.0 * scale;

        // Row positions
        let row1_y = dialog.y + title_h + padding;
        let row2_y = row1_y + line_h + row_gap;
        let row3_y = row2_y + line_h + row_gap;
        let row4_y = row3_y + line_h + row_gap;
        let row5_y = row4_y + line_h + row_gap;
        let row6_y = row5_y + line_h + row_gap;

        // Toggle bounds
        let (av_on, av_off) = self.toggle_bounds(&dialog, row1_y, line_h, scale);
        let (sp_normal, sp_fast) = self.toggle_bounds(&dialog, row2_y, line_h, scale);
        let (rs_normal, rs_large) = self.toggle_bounds(&dialog, row3_y, line_h, scale);
        let (wt_short, wt_full) = self.toggle_bounds(&dialog, row4_y, line_h, scale);
        let (ts_low, ts_normal, ts_high) = self.triple_toggle_bounds(&dialog, row5_y, line_h, scale);
        let (oc_on, oc_off) = self.toggle_bounds(&dialog, row6_y, line_h, scale);

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
            // Row size toggles
            if rs_normal.contains(*x, *y) {
                self.row_scale = 1.0;
                return EventResponse::Consumed;
            }
            if rs_large.contains(*x, *y) {
                self.row_scale = 1.5;
                return EventResponse::Consumed;
            }
            // Worktree name toggles
            if wt_short.contains(*x, *y) {
                self.abbreviate_worktree_names = true;
                return EventResponse::Consumed;
            }
            if wt_full.contains(*x, *y) {
                self.abbreviate_worktree_names = false;
                return EventResponse::Consumed;
            }
            // Time spacing toggles
            if ts_low.contains(*x, *y) {
                self.time_spacing_strength = 0.3;
                return EventResponse::Consumed;
            }
            if ts_normal.contains(*x, *y) {
                self.time_spacing_strength = 1.0;
                return EventResponse::Consumed;
            }
            if ts_high.contains(*x, *y) {
                self.time_spacing_strength = 2.0;
                return EventResponse::Consumed;
            }
            // Show orphans toggles
            if oc_on.contains(*x, *y) {
                self.show_orphaned_commits = true;
                return EventResponse::Consumed;
            }
            if oc_off.contains(*x, *y) {
                self.show_orphaned_commits = false;
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
        self.layout_with_bold(text_renderer, text_renderer, bounds)
    }
}

impl SettingsDialog {
    pub fn layout_with_bold(&self, text_renderer: &TextRenderer, bold_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
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
        let row_gap = 12.0 * scale;

        // Backdrop + shadow + dialog background
        create_dialog_backdrop(&mut output, &bounds, &dialog, scale);

        // Title (bold)
        let title_y = dialog.y + padding;
        output.bold_text_vertices.extend(bold_renderer.layout_text(
            "Settings",
            dialog.x + padding,
            title_y,
            theme::TEXT_BRIGHT.to_array(),
        ));

        // Title separator
        let sep_y = dialog.y + 36.0 * scale;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(dialog.x + padding, sep_y, dialog.width - padding * 2.0, 1.0),
            theme::BORDER.with_alpha(0.4).to_array(),
        ));

        // --- Setting rows ---
        let row1_y = dialog.y + title_h + padding;
        let row2_y = row1_y + line_h + row_gap;
        let row3_y = row2_y + line_h + row_gap;
        let row4_y = row3_y + line_h + row_gap;
        let row5_y = row4_y + line_h + row_gap;
        let row6_y = row5_y + line_h + row_gap;

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

        // Separator
        let sep1_y = row1_y + line_h + 5.0 * scale;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(dialog.x + padding, sep1_y, dialog.width - padding * 2.0, 1.0),
            theme::BORDER.to_array(),
        ));

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

        // Separator
        let sep2_y = row2_y + line_h + 5.0 * scale;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(dialog.x + padding, sep2_y, dialog.width - padding * 2.0, 1.0),
            theme::BORDER.to_array(),
        ));

        // Row 3: Row Size
        let label_y3 = row3_y + (line_h - line_height) / 2.0;
        output.text_vertices.extend(text_renderer.layout_text(
            "Row Size",
            dialog.x + padding,
            label_y3,
            theme::TEXT.to_array(),
        ));

        let (rs_normal, rs_large) = self.toggle_bounds(&dialog, row3_y, line_h, scale);
        self.render_toggle_option(&mut output, text_renderer, &rs_normal, "Normal", self.row_scale < 1.5);
        self.render_toggle_option(&mut output, text_renderer, &rs_large, "Large", self.row_scale >= 1.5);

        // Separator
        let sep3_y = row3_y + line_h + 5.0 * scale;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(dialog.x + padding, sep3_y, dialog.width - padding * 2.0, 1.0),
            theme::BORDER.to_array(),
        ));

        // Row 4: Worktree Names
        let label_y4 = row4_y + (line_h - line_height) / 2.0;
        output.text_vertices.extend(text_renderer.layout_text(
            "Worktree Names",
            dialog.x + padding,
            label_y4,
            theme::TEXT.to_array(),
        ));

        let (wt_short, wt_full) = self.toggle_bounds(&dialog, row4_y, line_h, scale);
        self.render_toggle_option(&mut output, text_renderer, &wt_short, "Short", self.abbreviate_worktree_names);
        self.render_toggle_option(&mut output, text_renderer, &wt_full, "Full", !self.abbreviate_worktree_names);

        // Separator
        let sep4_y = row4_y + line_h + 5.0 * scale;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(dialog.x + padding, sep4_y, dialog.width - padding * 2.0, 1.0),
            theme::BORDER.to_array(),
        ));

        // Row 5: Time Spacing
        let label_y5 = row5_y + (line_h - line_height) / 2.0;
        output.text_vertices.extend(text_renderer.layout_text(
            "Time Spacing",
            dialog.x + padding,
            label_y5,
            theme::TEXT.to_array(),
        ));

        let (ts_low, ts_normal, ts_high) = self.triple_toggle_bounds(&dialog, row5_y, line_h, scale);
        self.render_toggle_option(&mut output, text_renderer, &ts_low, "Low", self.time_spacing_strength < 0.5);
        self.render_toggle_option(&mut output, text_renderer, &ts_normal, "Normal", self.time_spacing_strength >= 0.5 && self.time_spacing_strength < 1.5);
        self.render_toggle_option(&mut output, text_renderer, &ts_high, "High", self.time_spacing_strength >= 1.5);

        // Separator
        let sep5_y = row5_y + line_h + 5.0 * scale;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(dialog.x + padding, sep5_y, dialog.width - padding * 2.0, 1.0),
            theme::BORDER.to_array(),
        ));

        // Row 6: Show Orphans
        let label_y6 = row6_y + (line_h - line_height) / 2.0;
        output.text_vertices.extend(text_renderer.layout_text(
            "Show Orphans",
            dialog.x + padding,
            label_y6,
            theme::TEXT.to_array(),
        ));

        let (oc_on, oc_off) = self.toggle_bounds(&dialog, row6_y, line_h, scale);
        self.render_toggle_option(&mut output, text_renderer, &oc_on, "ON", self.show_orphaned_commits);
        self.render_toggle_option(&mut output, text_renderer, &oc_off, "OFF", !self.show_orphaned_commits);

        // Close button at bottom
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let close_x = dialog.right() - padding - button_w;

        // Button separator
        let btn_sep_y = button_y - 8.0 * scale;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(dialog.x + padding, btn_sep_y, dialog.width - padding * 2.0, 1.0),
            theme::BORDER.with_alpha(0.4).to_array(),
        ));

        let close_bounds = Rect::new(close_x, button_y, button_w, line_h);
        output.extend(self.close_button.layout(text_renderer, close_bounds));

        // Version text at bottom-left
        let version_y = button_y + (line_h - line_height) / 2.0;
        output.text_vertices.extend(text_renderer.layout_text(
            "whisper-git v0.1.0",
            dialog.x + padding,
            version_y,
            theme::TEXT_MUTED.to_array(),
        ));

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
        let toggle_radius = (rect.height * 0.15).min(4.0);
        output.spline_vertices.extend(create_rounded_rect_vertices(rect, bg_color, toggle_radius));

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
