//! Confirmation dialog - modal overlay for confirming destructive actions

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{
    create_dialog_backdrop, theme, Widget, WidgetOutput,
};
use crate::ui::widgets::Button;
use crate::ui::{Rect, TextRenderer};

/// Actions from the confirm dialog
#[derive(Clone, Debug)]
pub enum ConfirmDialogAction {
    Confirm,
    Cancel,
}

/// A modal confirmation dialog with OK and Cancel buttons
pub struct ConfirmDialog {
    visible: bool,
    title: String,
    message: String,
    ok_button: Button,
    cancel_button: Button,
    pending_action: Option<ConfirmDialogAction>,
}

impl ConfirmDialog {
    pub fn new() -> Self {
        Self {
            visible: false,
            title: String::new(),
            message: String::new(),
            ok_button: Button::new("OK").primary(),
            cancel_button: Button::new("Cancel"),
            pending_action: None,
        }
    }

    pub fn show(&mut self, title: &str, message: &str) {
        self.visible = true;
        self.title = title.to_string();
        self.message = message.to_string();
        self.pending_action = None;
    }

    pub fn hide(&mut self) {
        self.visible = false;
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn take_action(&mut self) -> Option<ConfirmDialogAction> {
        self.pending_action.take()
    }

    /// Compute dialog bounds centered in screen
    fn dialog_bounds(&self, screen: Rect, scale: f32) -> Rect {
        let dialog_w = (360.0 * scale).min(screen.width * 0.8);
        let dialog_h = (150.0 * scale).min(screen.height * 0.5);
        let dialog_x = screen.x + (screen.width - dialog_w) / 2.0;
        let dialog_y = screen.y + (screen.height - dialog_h) / 2.0;
        Rect::new(dialog_x, dialog_y, dialog_w, dialog_h)
    }
}

impl Widget for ConfirmDialog {
    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        if !self.visible {
            return EventResponse::Ignored;
        }

        let scale = (bounds.height / 720.0).max(1.0);
        let dialog = self.dialog_bounds(bounds, scale);
        let padding = 16.0 * scale;
        let line_h = 32.0 * scale;

        // Button bounds
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let button_gap = 8.0 * scale;
        let cancel_x = dialog.right() - padding - button_w;
        let ok_x = cancel_x - button_w - button_gap;
        let ok_bounds = Rect::new(ok_x, button_y, button_w, line_h);
        let cancel_bounds = Rect::new(cancel_x, button_y, button_w, line_h);

        // Handle keyboard shortcuts
        if let InputEvent::KeyDown { key, .. } = event {
            match key {
                Key::Escape => {
                    self.pending_action = Some(ConfirmDialogAction::Cancel);
                    self.hide();
                    return EventResponse::Consumed;
                }
                Key::Enter => {
                    self.pending_action = Some(ConfirmDialogAction::Confirm);
                    self.hide();
                    return EventResponse::Consumed;
                }
                _ => {}
            }
        }

        // Route to buttons
        if self.ok_button.handle_event(event, ok_bounds).is_consumed() {
            if self.ok_button.was_clicked() {
                self.pending_action = Some(ConfirmDialogAction::Confirm);
                self.hide();
            }
            return EventResponse::Consumed;
        }

        if self.cancel_button.handle_event(event, cancel_bounds).is_consumed() {
            if self.cancel_button.was_clicked() {
                self.pending_action = Some(ConfirmDialogAction::Cancel);
                self.hide();
            }
            return EventResponse::Consumed;
        }

        // Click outside dialog dismisses (cancel)
        if let InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } = event
            && !dialog.contains(*x, *y) {
                self.pending_action = Some(ConfirmDialogAction::Cancel);
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

        // Backdrop + shadow + dialog background
        create_dialog_backdrop(&mut output, &bounds, &dialog, scale);

        // Title
        let title_y = dialog.y + padding;
        output.text_vertices.extend(text_renderer.layout_text(
            &self.title,
            dialog.x + padding,
            title_y,
            theme::TEXT_BRIGHT.to_array(),
        ));

        // Message
        let message_y = dialog.y + 44.0 * scale;
        output.text_vertices.extend(text_renderer.layout_text(
            &self.message,
            dialog.x + padding,
            message_y,
            theme::TEXT.to_array(),
        ));

        // Buttons at bottom
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let button_gap = 8.0 * scale;
        let cancel_x = dialog.right() - padding - button_w;
        let ok_x = cancel_x - button_w - button_gap;

        let ok_bounds = Rect::new(ok_x, button_y, button_w, line_h);
        let cancel_bounds = Rect::new(cancel_x, button_y, button_w, line_h);

        output.extend(self.ok_button.layout(text_renderer, ok_bounds));
        output.extend(self.cancel_button.layout(text_renderer, cancel_bounds));

        output
    }
}
