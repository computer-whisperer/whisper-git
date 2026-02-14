//! Pull dialog - modal overlay for pulling any branch from any remote with optional rebase

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{
    create_dialog_backdrop, theme, Widget, WidgetOutput,
};
use crate::ui::widgets::{Button, TextInput};
use crate::ui::{Rect, TextRenderer};

/// Actions from the pull dialog
#[derive(Clone, Debug)]
pub enum PullDialogAction {
    /// Pull confirmed with (remote, branch, rebase)
    Confirm {
        remote: String,
        branch: String,
        rebase: bool,
    },
    /// User cancelled
    Cancel,
}

/// A modal dialog for pulling branches from remotes
pub struct PullDialog {
    visible: bool,
    remote_input: TextInput,
    branch_input: TextInput,
    rebase: bool,
    pull_button: Button,
    cancel_button: Button,
    pending_action: Option<PullDialogAction>,
    /// Index of the focused field (0=remote, 1=branch)
    focused_field: usize,
}

impl PullDialog {
    pub fn new() -> Self {
        Self {
            visible: false,
            remote_input: TextInput::new().with_placeholder("origin"),
            branch_input: TextInput::new().with_placeholder("main"),
            rebase: false,
            pull_button: Button::new("Pull").primary(),
            cancel_button: Button::new("Cancel"),
            pending_action: None,
            focused_field: 0,
        }
    }

    /// Show the dialog with pre-filled defaults
    pub fn show(&mut self, current_branch: &str, default_remote: &str) {
        self.visible = true;
        self.remote_input = TextInput::new().with_placeholder("origin");
        self.remote_input.set_text(default_remote);
        self.remote_input.set_focused(true);
        self.branch_input = TextInput::new().with_placeholder("main");
        self.branch_input.set_text(current_branch);
        self.branch_input.set_focused(false);
        self.rebase = false;
        self.focused_field = 0;
        self.pending_action = None;
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.remote_input.set_focused(false);
        self.branch_input.set_focused(false);
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn take_action(&mut self) -> Option<PullDialogAction> {
        self.pending_action.take()
    }

    fn try_confirm(&mut self) {
        let remote = self.remote_input.text().trim().to_string();
        let branch = self.branch_input.text().trim().to_string();

        if remote.is_empty() || branch.is_empty() {
            return;
        }

        self.pending_action = Some(PullDialogAction::Confirm {
            remote,
            branch,
            rebase: self.rebase,
        });
        self.hide();
    }

    /// Compute dialog bounds centered in screen
    fn dialog_bounds(&self, screen: Rect, scale: f32) -> Rect {
        let dialog_w = (400.0 * scale).min(screen.width * 0.85);
        let dialog_h = (220.0 * scale).min(screen.height * 0.65);
        let dialog_x = screen.x + (screen.width - dialog_w) / 2.0;
        let dialog_y = screen.y + (screen.height - dialog_h) / 2.0;
        Rect::new(dialog_x, dialog_y, dialog_w, dialog_h)
    }

    /// Update focus between the two text inputs
    fn update_focus(&mut self) {
        self.remote_input.set_focused(self.focused_field == 0);
        self.branch_input.set_focused(self.focused_field == 1);
    }
}

impl Widget for PullDialog {
    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        if !self.visible {
            return EventResponse::Ignored;
        }

        let scale = (bounds.height / 720.0).max(1.0);
        let dialog = self.dialog_bounds(bounds, scale);
        let padding = 16.0 * scale;
        let line_h = 32.0 * scale;
        let label_h = 18.0 * scale;

        // Remote input bounds
        let remote_label_y = dialog.y + 40.0 * scale;
        let remote_input_y = remote_label_y + label_h;
        let input_w = dialog.width - padding * 2.0;
        let remote_input_bounds = Rect::new(
            dialog.x + padding,
            remote_input_y,
            input_w,
            line_h,
        );

        // Branch input bounds
        let branch_label_y = remote_input_y + line_h + 8.0 * scale;
        let branch_input_y = branch_label_y + label_h;
        let branch_input_bounds = Rect::new(
            dialog.x + padding,
            branch_input_y,
            input_w,
            line_h,
        );

        // Checkbox bounds
        let checkbox_y = branch_input_y + line_h + 8.0 * scale;
        let checkbox_size = 16.0 * scale;
        let checkbox_bounds = Rect::new(
            dialog.x + padding,
            checkbox_y,
            checkbox_size,
            checkbox_size,
        );

        // Button bounds
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let button_gap = 8.0 * scale;
        let cancel_x = dialog.right() - padding - button_w;
        let confirm_x = cancel_x - button_w - button_gap;
        let confirm_bounds = Rect::new(confirm_x, button_y, button_w, line_h);
        let cancel_bounds = Rect::new(cancel_x, button_y, button_w, line_h);

        // Handle keyboard shortcuts first
        if let InputEvent::KeyDown { key, .. } = event {
            match key {
                Key::Escape => {
                    self.pending_action = Some(PullDialogAction::Cancel);
                    self.hide();
                    return EventResponse::Consumed;
                }
                Key::Enter => {
                    self.try_confirm();
                    return EventResponse::Consumed;
                }
                Key::Tab => {
                    self.focused_field = (self.focused_field + 1) % 2;
                    self.update_focus();
                    return EventResponse::Consumed;
                }
                _ => {}
            }
        }

        // Handle click-to-focus between fields
        if let InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } = event {
            if remote_input_bounds.contains(*x, *y) {
                self.focused_field = 0;
                self.update_focus();
            } else if branch_input_bounds.contains(*x, *y) {
                self.focused_field = 1;
                self.update_focus();
            } else if checkbox_bounds.contains(*x, *y) {
                self.rebase = !self.rebase;
                return EventResponse::Consumed;
            }
        }

        // Route to text inputs
        if self.remote_input.handle_event(event, remote_input_bounds).is_consumed() {
            return EventResponse::Consumed;
        }
        if self.branch_input.handle_event(event, branch_input_bounds).is_consumed() {
            return EventResponse::Consumed;
        }

        // Route to buttons
        if self.pull_button.handle_event(event, confirm_bounds).is_consumed() {
            if self.pull_button.was_clicked() {
                self.try_confirm();
            }
            return EventResponse::Consumed;
        }

        if self.cancel_button.handle_event(event, cancel_bounds).is_consumed() {
            if self.cancel_button.was_clicked() {
                self.pending_action = Some(PullDialogAction::Cancel);
                self.hide();
            }
            return EventResponse::Consumed;
        }

        // Click outside dialog dismisses (cancel)
        if let InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } = event
            && !dialog.contains(*x, *y) {
                self.pending_action = Some(PullDialogAction::Cancel);
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
        let label_h = 18.0 * scale;

        // Backdrop + shadow + dialog background
        create_dialog_backdrop(&mut output, &bounds, &dialog, scale);

        // Title
        let title_y = dialog.y + padding;
        output.bold_text_vertices.extend(text_renderer.layout_text(
            "Pull Branch",
            dialog.x + padding,
            title_y,
            theme::TEXT_BRIGHT.to_array(),
        ));

        // Remote label + input
        let remote_label_y = dialog.y + 40.0 * scale;
        output.text_vertices.extend(text_renderer.layout_text(
            "Remote:",
            dialog.x + padding,
            remote_label_y,
            theme::TEXT_MUTED.to_array(),
        ));
        let remote_input_y = remote_label_y + label_h;
        let input_w = dialog.width - padding * 2.0;
        let remote_input_bounds = Rect::new(
            dialog.x + padding,
            remote_input_y,
            input_w,
            line_h,
        );
        output.extend(self.remote_input.layout(text_renderer, remote_input_bounds));

        // Remote branch label + input
        let branch_label_y = remote_input_y + line_h + 8.0 * scale;
        output.text_vertices.extend(text_renderer.layout_text(
            "Remote branch:",
            dialog.x + padding,
            branch_label_y,
            theme::TEXT_MUTED.to_array(),
        ));
        let branch_input_y = branch_label_y + label_h;
        let branch_input_bounds = Rect::new(
            dialog.x + padding,
            branch_input_y,
            input_w,
            line_h,
        );
        output.extend(self.branch_input.layout(text_renderer, branch_input_bounds));

        // Checkbox + label
        let checkbox_y = branch_input_y + line_h + 8.0 * scale;
        let checkbox_size = 16.0 * scale;
        let checkbox_x = dialog.x + padding;

        // Draw checkbox box
        use crate::ui::widget::create_rect_outline_vertices;
        let checkbox_rect = Rect::new(checkbox_x, checkbox_y, checkbox_size, checkbox_size);
        output.spline_vertices.extend(create_rect_outline_vertices(
            &checkbox_rect,
            theme::BORDER.to_array(),
            1.0 * scale,
        ));

        // Draw checkmark if checked
        if self.rebase {
            use crate::ui::widget::create_rect_vertices;
            let check_padding = 3.0 * scale;
            let check_rect = Rect::new(
                checkbox_x + check_padding,
                checkbox_y + check_padding,
                checkbox_size - check_padding * 2.0,
                checkbox_size - check_padding * 2.0,
            );
            output.spline_vertices.extend(create_rect_vertices(
                &check_rect,
                theme::ACCENT.to_array(),
            ));
        }

        // Checkbox label
        let checkbox_label_x = checkbox_x + checkbox_size + 8.0 * scale;
        output.text_vertices.extend(text_renderer.layout_text(
            "Rebase instead of merge",
            checkbox_label_x,
            checkbox_y,
            theme::TEXT.to_array(),
        ));

        // Buttons at bottom
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let button_gap = 8.0 * scale;
        let cancel_x = dialog.right() - padding - button_w;
        let confirm_x = cancel_x - button_w - button_gap;

        let confirm_bounds = Rect::new(confirm_x, button_y, button_w, line_h);
        let cancel_bounds = Rect::new(cancel_x, button_y, button_w, line_h);

        output.extend(self.pull_button.layout(text_renderer, confirm_bounds));
        output.extend(self.cancel_button.layout(text_renderer, cancel_bounds));

        output
    }
}
