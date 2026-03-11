//! Error dialog - modal overlay for displaying git operation errors with full detail

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::text_util::wrap_text;
use crate::ui::widget::{
    Widget, WidgetOutput, create_dialog_backdrop, create_rect_vertices, theme,
};
use crate::ui::widgets::Button;
use crate::ui::{Rect, TextRenderer};

/// A modal error dialog showing a title, summary, full error detail, and dismiss button
pub struct ErrorDialog {
    visible: bool,
    title: String,
    summary: String,
    detail: String,
    dismiss_button: Button,
    /// Scroll offset for the detail text area (in pixels)
    scroll_offset: f32,
}

impl ErrorDialog {
    pub fn new() -> Self {
        Self {
            visible: false,
            title: String::new(),
            summary: String::new(),
            detail: String::new(),
            dismiss_button: Button::new("Dismiss"),
            scroll_offset: 0.0,
        }
    }

    /// Show the error dialog with a friendly summary and the raw git stderr.
    pub fn show(&mut self, title: &str, summary: &str, detail: &str) {
        self.visible = true;
        self.title = title.to_string();
        self.summary = summary.to_string();
        self.detail = detail.trim().to_string();
        self.scroll_offset = 0.0;
    }

    pub fn hide(&mut self) {
        self.visible = false;
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn layout_with_bold(
        &self,
        text_renderer: &TextRenderer,
        bold_renderer: &TextRenderer,
        bounds: Rect,
    ) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        if !self.visible {
            return output;
        }

        let scale = (bounds.height / 720.0).max(1.0);
        let dialog = self.dialog_bounds(bounds, scale);
        let padding = 16.0 * scale;
        let line_h = text_renderer.line_height();
        let btn_h = 32.0 * scale;

        // Backdrop + shadow + dialog background
        create_dialog_backdrop(&mut output, &bounds, &dialog, scale);

        // Title (bold, error red)
        let title_y = dialog.y + padding;
        let error_title_color = [0.937, 0.325, 0.314, 1.0]; // red matching toast error border
        output.bold_text_vertices.extend(bold_renderer.layout_text(
            &self.title,
            dialog.x + padding,
            title_y,
            error_title_color,
        ));

        // Title separator
        let sep_y = dialog.y + padding + line_h + 6.0 * scale;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(dialog.x + padding, sep_y, dialog.width - padding * 2.0, 1.0),
            theme::BORDER.with_alpha(0.4).to_array(),
        ));

        // Summary text (wrapped)
        let summary_y = sep_y + 8.0 * scale;
        let content_width = dialog.width - padding * 2.0;
        let summary_lines = wrap_text(&self.summary, content_width, text_renderer);
        for (i, line) in summary_lines.iter().enumerate() {
            output.text_vertices.extend(text_renderer.layout_text(
                line,
                dialog.x + padding,
                summary_y + i as f32 * line_h,
                theme::TEXT_BRIGHT.to_array(),
            ));
        }
        let summary_bottom = summary_y + summary_lines.len().max(1) as f32 * line_h;
        let btn_sep_y = dialog.bottom() - padding - btn_h - 12.0 * scale;

        // Detail section (only if there's git output to show)
        if !self.detail.is_empty() {
            let detail_label_y = summary_bottom + 8.0 * scale;
            output.text_vertices.extend(text_renderer.layout_text(
                "Git output:",
                dialog.x + padding,
                detail_label_y,
                theme::TEXT_MUTED.to_array(),
            ));

            // Detail area background (darker inset)
            let detail_top = detail_label_y + line_h + 4.0 * scale;
            let detail_area_height = (btn_sep_y - 4.0 * scale - detail_top).max(line_h * 2.0);
            let detail_rect = Rect::new(
                dialog.x + padding,
                detail_top,
                content_width,
                detail_area_height,
            );

            output.spline_vertices.extend(create_rect_vertices(
                &detail_rect,
                theme::BACKGROUND.to_array(),
            ));

            // Detail text (wrapped, scrollable, clipped to detail_rect)
            let detail_inner_pad = 6.0 * scale;
            let detail_text_width = content_width - detail_inner_pad * 2.0;
            let detail_lines = wrap_text(&self.detail, detail_text_width, text_renderer);
            let total_text_height = detail_lines.len() as f32 * line_h;
            let viewport_h = detail_area_height - detail_inner_pad * 2.0;

            // Clip: only render lines visible within the detail_rect
            let text_start_y = detail_rect.y + detail_inner_pad - self.scroll_offset;
            for (i, line) in detail_lines.iter().enumerate() {
                let ly = text_start_y + i as f32 * line_h;
                if ly + line_h < detail_rect.y || ly > detail_rect.bottom() {
                    continue;
                }
                output.text_vertices.extend(text_renderer.layout_text(
                    line,
                    detail_rect.x + detail_inner_pad,
                    ly,
                    theme::TEXT.to_array(),
                ));
            }

            // Scrollbar indicator if content overflows
            if total_text_height > viewport_h {
                let scrollbar_w = 3.0 * scale;
                let scrollbar_x = detail_rect.right() - scrollbar_w - 2.0 * scale;
                let max_scroll = total_text_height - viewport_h;
                let scroll_frac = if max_scroll > 0.0 {
                    self.scroll_offset / max_scroll
                } else {
                    0.0
                };
                let thumb_h =
                    (viewport_h / total_text_height * detail_area_height).max(12.0 * scale);
                let thumb_y = detail_rect.y + scroll_frac * (detail_area_height - thumb_h);
                output.spline_vertices.extend(create_rect_vertices(
                    &Rect::new(scrollbar_x, thumb_y, scrollbar_w, thumb_h),
                    theme::TEXT_MUTED.with_alpha(0.4).to_array(),
                ));
            }
        }

        // Button separator
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(
                dialog.x + padding,
                btn_sep_y,
                dialog.width - padding * 2.0,
                1.0,
            ),
            theme::BORDER.with_alpha(0.4).to_array(),
        ));

        // Dismiss button at bottom right
        let button_y = btn_sep_y + 8.0 * scale;
        let button_w = 90.0 * scale;
        let dismiss_x = dialog.right() - padding - button_w;
        let dismiss_bounds = Rect::new(dismiss_x, button_y, button_w, btn_h);
        output.extend(self.dismiss_button.layout(text_renderer, dismiss_bounds));

        output
    }

    /// Compute dialog bounds centered in screen — taller than confirm dialog to fit detail
    fn dialog_bounds(&self, screen: Rect, scale: f32) -> Rect {
        let dialog_w = (480.0 * scale).min(screen.width * 0.85);
        let dialog_h = (380.0 * scale).min(screen.height * 0.7);
        let dialog_x = screen.x + (screen.width - dialog_w) / 2.0;
        let dialog_y = screen.y + (screen.height - dialog_h) / 2.0;
        Rect::new(dialog_x, dialog_y, dialog_w, dialog_h)
    }
}

impl Widget for ErrorDialog {
    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        if !self.visible {
            return EventResponse::Ignored;
        }

        let scale = (bounds.height / 720.0).max(1.0);
        let dialog = self.dialog_bounds(bounds, scale);
        let padding = 16.0 * scale;
        let btn_h = 32.0 * scale;

        // Dismiss button bounds
        let btn_sep_y = dialog.bottom() - padding - btn_h - 12.0 * scale;
        let button_y = btn_sep_y + 8.0 * scale;
        let button_w = 90.0 * scale;
        let dismiss_x = dialog.right() - padding - button_w;
        let dismiss_bounds = Rect::new(dismiss_x, button_y, button_w, btn_h);

        // Keyboard shortcuts
        if let InputEvent::KeyDown { key, .. } = event
            && matches!(key, Key::Escape | Key::Enter)
        {
            self.hide();
            return EventResponse::Consumed;
        }

        // Scroll in detail area
        if let InputEvent::Scroll { x, y, delta_y, .. } = event
            && dialog.contains(*x, *y)
        {
            let line_h = 18.0 * scale; // approximate line height for scrolling
            self.scroll_offset = (self.scroll_offset - delta_y * line_h * 3.0).max(0.0);
            self.scroll_offset = self.scroll_offset.min(10000.0);
            return EventResponse::Consumed;
        }

        // Dismiss button
        if self
            .dismiss_button
            .handle_event(event, dismiss_bounds)
            .is_consumed()
        {
            if self.dismiss_button.was_clicked() {
                self.hide();
            }
            return EventResponse::Consumed;
        }

        // Click outside dialog dismisses
        if let InputEvent::MouseDown {
            button: MouseButton::Left,
            x,
            y,
            ..
        } = event
            && !dialog.contains(*x, *y)
        {
            self.hide();
            return EventResponse::Consumed;
        }

        // Consume all events while visible (modal)
        EventResponse::Consumed
    }

    fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        self.layout_with_bold(text_renderer, text_renderer, bounds)
    }
}
