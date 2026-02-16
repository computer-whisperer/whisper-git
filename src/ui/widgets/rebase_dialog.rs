//! Rebase dialog - modal overlay for choosing rebase options (--autostash, --rebase-merges)

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{
    create_dialog_backdrop, create_rect_outline_vertices, create_rect_vertices, theme, Widget,
    WidgetOutput,
};
use crate::ui::widgets::Button;
use crate::ui::{Rect, TextRenderer};

/// Options for the rebase operation
#[derive(Clone, Copy, Debug)]
pub struct RebaseOptions {
    pub autostash: bool,
    pub rebase_merges: bool,
}

/// Actions from the rebase dialog
#[derive(Clone, Debug)]
pub enum RebaseDialogAction {
    /// Confirm rebase with (branch, options)
    Confirm(String, RebaseOptions),
    /// User cancelled
    Cancel,
}

/// A modal dialog for choosing rebase options
pub struct RebaseDialog {
    visible: bool,
    /// The branch being rebased onto
    target_branch: String,
    /// The current branch (being rebased)
    current_branch: String,
    /// Auto-stash uncommitted changes
    autostash: bool,
    /// Preserve merge commits
    rebase_merges: bool,
    /// Confirm (Rebase) button
    rebase_button: Button,
    /// Cancel button
    cancel_button: Button,
    /// Pending action to be consumed
    pending_action: Option<RebaseDialogAction>,
    /// Warning text to display (e.g. dirty workdir)
    warning: Option<String>,
    /// Number of uncommitted changes (for warning display)
    uncommitted_count: usize,
}

impl RebaseDialog {
    pub fn new() -> Self {
        Self {
            visible: false,
            target_branch: String::new(),
            current_branch: String::new(),
            autostash: false,
            rebase_merges: false,
            rebase_button: Button::new("Rebase").primary(),
            cancel_button: Button::new("Cancel"),
            pending_action: None,
            warning: None,
            uncommitted_count: 0,
        }
    }

    /// Show the dialog for rebasing onto a branch.
    /// `uncommitted` is the number of uncommitted changes (for warning display).
    pub fn show(&mut self, target_branch: &str, current_branch: &str, uncommitted: usize) {
        self.visible = true;
        self.target_branch = target_branch.to_string();
        self.current_branch = current_branch.to_string();
        self.autostash = false;
        self.rebase_merges = false;
        self.uncommitted_count = uncommitted;
        self.rebase_button = Button::new("Rebase").primary();
        self.pending_action = None;

        if uncommitted > 0 {
            self.warning = Some(format!(
                "Warning: {} uncommitted change{}. Stash or commit first.",
                uncommitted,
                if uncommitted == 1 { "" } else { "s" },
            ));
        } else {
            self.warning = None;
        }
    }

    pub fn hide(&mut self) {
        self.visible = false;
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn take_action(&mut self) -> Option<RebaseDialogAction> {
        self.pending_action.take()
    }

    fn try_confirm(&mut self) {
        self.pending_action = Some(RebaseDialogAction::Confirm(
            self.target_branch.clone(),
            RebaseOptions {
                autostash: self.autostash,
                rebase_merges: self.rebase_merges,
            },
        ));
        self.hide();
    }

    /// Whether to show the uncommitted warning (only when uncommitted > 0 and autostash is off)
    fn show_warning(&self) -> bool {
        self.uncommitted_count > 0 && !self.autostash
    }

    /// Compute dialog bounds centered in screen
    fn dialog_bounds(&self, screen: Rect, scale: f32) -> Rect {
        let dialog_w = (420.0 * scale).min(screen.width * 0.85);
        let warning_h = if self.show_warning() { 24.0 } else { 0.0 };
        let dialog_h = ((200.0 + warning_h) * scale).min(screen.height * 0.8);
        let dialog_x = screen.x + (screen.width - dialog_w) / 2.0;
        let dialog_y = screen.y + (screen.height - dialog_h) / 2.0;
        Rect::new(dialog_x, dialog_y, dialog_w, dialog_h)
    }

    /// Checkbox items: (field_index, label)
    fn checkbox_items() -> &'static [(usize, &'static str)] {
        &[
            (0, "Auto-stash uncommitted changes (--autostash)"),
            (1, "Preserve merge commits (--rebase-merges)"),
        ]
    }

    fn get_checkbox(&self, index: usize) -> bool {
        match index {
            0 => self.autostash,
            1 => self.rebase_merges,
            _ => false,
        }
    }

    fn toggle_checkbox(&mut self, index: usize) {
        match index {
            0 => self.autostash = !self.autostash,
            1 => self.rebase_merges = !self.rebase_merges,
            _ => {}
        }
    }
}

impl Widget for RebaseDialog {
    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        if !self.visible {
            return EventResponse::Ignored;
        }

        let scale = (bounds.height / 720.0).max(1.0);
        let dialog = self.dialog_bounds(bounds, scale);
        let padding = 16.0 * scale;
        let line_h = 32.0 * scale;
        let checkbox_line_h = 24.0 * scale;

        // Handle keyboard shortcuts first
        if let InputEvent::KeyDown { key, .. } = event {
            match key {
                Key::Escape => {
                    self.pending_action = Some(RebaseDialogAction::Cancel);
                    self.hide();
                    return EventResponse::Consumed;
                }
                Key::Enter => {
                    self.try_confirm();
                    return EventResponse::Consumed;
                }
                _ => {}
            }
        }

        // Checkbox click detection
        if let InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } = event {
            let checkbox_start_y = dialog.y + 60.0 * scale;
            let items = Self::checkbox_items();
            for (i, (field_idx, _label)) in items.iter().enumerate() {
                let item_y = checkbox_start_y + i as f32 * checkbox_line_h;
                let item_bounds = Rect::new(
                    dialog.x + padding,
                    item_y,
                    dialog.width - padding * 2.0,
                    checkbox_line_h,
                );
                if item_bounds.contains(*x, *y) {
                    self.toggle_checkbox(*field_idx);
                    return EventResponse::Consumed;
                }
            }
        }

        // Button bounds
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let button_gap = 8.0 * scale;
        let cancel_x = dialog.right() - padding - button_w;
        let rebase_x = cancel_x - button_w - button_gap;
        let rebase_bounds = Rect::new(rebase_x, button_y, button_w, line_h);
        let cancel_bounds = Rect::new(cancel_x, button_y, button_w, line_h);

        if self.rebase_button.handle_event(event, rebase_bounds).is_consumed() {
            if self.rebase_button.was_clicked() {
                self.try_confirm();
            }
            return EventResponse::Consumed;
        }

        if self.cancel_button.handle_event(event, cancel_bounds).is_consumed() {
            if self.cancel_button.was_clicked() {
                self.pending_action = Some(RebaseDialogAction::Cancel);
                self.hide();
            }
            return EventResponse::Consumed;
        }

        // Click outside dialog dismisses (cancel)
        if let InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } = event
            && !dialog.contains(*x, *y)
        {
            self.pending_action = Some(RebaseDialogAction::Cancel);
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
        let checkbox_line_h = 24.0 * scale;

        // Backdrop + shadow + dialog background
        create_dialog_backdrop(&mut output, &bounds, &dialog, scale);

        // Title (bold)
        let title = format!("Rebase onto '{}'", self.target_branch);
        let title_y = dialog.y + padding;
        output.bold_text_vertices.extend(text_renderer.layout_text(
            &title,
            dialog.x + padding,
            title_y,
            theme::TEXT_BRIGHT.to_array(),
        ));

        // Subtitle (muted)
        let subtitle = format!("Rebasing: {}", self.current_branch);
        let subtitle_y = dialog.y + 40.0 * scale;
        output.text_vertices.extend(text_renderer.layout_text(
            &subtitle,
            dialog.x + padding,
            subtitle_y,
            theme::TEXT_MUTED.to_array(),
        ));

        // Checkboxes
        let checkbox_start_y = dialog.y + 60.0 * scale;
        let checkbox_size = 14.0 * scale;
        let items = Self::checkbox_items();

        for (i, (field_idx, label)) in items.iter().enumerate() {
            let item_y = checkbox_start_y + i as f32 * checkbox_line_h;
            let checked = self.get_checkbox(*field_idx);

            let cb_x = dialog.x + padding;
            let cb_y = item_y + (checkbox_line_h - checkbox_size) / 2.0;

            // Draw checkbox outline
            let cb_rect = Rect::new(cb_x, cb_y, checkbox_size, checkbox_size);
            let border_color = if checked {
                theme::ACCENT.to_array()
            } else {
                theme::BORDER.to_array()
            };
            output
                .spline_vertices
                .extend(create_rect_outline_vertices(&cb_rect, border_color, 1.0 * scale));

            // Draw filled inner rect when checked
            if checked {
                let check_padding = 3.0 * scale;
                let check_rect = Rect::new(
                    cb_x + check_padding,
                    cb_y + check_padding,
                    checkbox_size - check_padding * 2.0,
                    checkbox_size - check_padding * 2.0,
                );
                output
                    .spline_vertices
                    .extend(create_rect_vertices(&check_rect, theme::ACCENT.to_array()));
            }

            // Label text
            let text_x = cb_x + checkbox_size + 8.0 * scale;
            let text_color = if checked {
                theme::TEXT_BRIGHT
            } else {
                theme::TEXT
            };
            output.text_vertices.extend(text_renderer.layout_text(
                label,
                text_x,
                item_y + 4.0 * scale,
                text_color.to_array(),
            ));
        }

        // Warning text (amber) - only when uncommitted > 0 and autostash is off
        if self.show_warning() {
            if let Some(ref warning) = self.warning {
                let warning_y = dialog.bottom() - padding - line_h - 22.0 * scale;
                let amber = [1.0, 0.718, 0.302, 1.0]; // #FFB74D
                output.text_vertices.extend(text_renderer.layout_text(
                    warning,
                    dialog.x + padding,
                    warning_y,
                    amber,
                ));
            }
        }

        // Buttons at bottom
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let button_gap = 8.0 * scale;
        let cancel_x = dialog.right() - padding - button_w;
        let rebase_x = cancel_x - button_w - button_gap;

        let rebase_bounds = Rect::new(rebase_x, button_y, button_w, line_h);
        let cancel_bounds = Rect::new(cancel_x, button_y, button_w, line_h);

        output.extend(self.rebase_button.layout(text_renderer, rebase_bounds));
        output.extend(self.cancel_button.layout(text_renderer, cancel_bounds));

        output
    }
}
