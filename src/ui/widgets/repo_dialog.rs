//! Repository open dialog - modal overlay for entering a repo path

use std::path::PathBuf;

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{
    create_rect_vertices, create_rect_outline_vertices, theme, Widget, WidgetId, WidgetOutput, WidgetState,
};
use crate::ui::widgets::{Button, TextInput};
use crate::ui::{Rect, TextRenderer};

/// Actions from the repo dialog
#[derive(Clone, Debug)]
pub enum RepoDialogAction {
    Open(PathBuf),
    Cancel,
}

/// A modal dialog for opening a repository by path
#[allow(dead_code)]
pub struct RepoDialog {
    id: WidgetId,
    state: WidgetState,
    visible: bool,
    path_input: TextInput,
    open_button: Button,
    cancel_button: Button,
    error_message: Option<String>,
    pending_action: Option<RepoDialogAction>,
}

impl RepoDialog {
    pub fn new() -> Self {
        Self {
            id: WidgetId::new(),
            state: WidgetState::new(),
            visible: false,
            path_input: TextInput::new().with_placeholder("/path/to/repository"),
            open_button: Button::new("Open").primary(),
            cancel_button: Button::new("Cancel"),
            error_message: None,
            pending_action: None,
        }
    }

    pub fn show(&mut self) {
        self.visible = true;
        self.path_input.set_text("");
        self.path_input.set_focused(true);
        self.error_message = None;
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.path_input.set_focused(false);
        self.error_message = None;
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn take_action(&mut self) -> Option<RepoDialogAction> {
        self.pending_action.take()
    }

    fn try_open(&mut self) {
        let path_str = self.path_input.text().trim().to_string();
        if path_str.is_empty() {
            self.error_message = Some("Please enter a path".to_string());
            return;
        }

        let path = PathBuf::from(&path_str);

        // Validate it's a git repo by trying to discover
        match git2::Repository::discover(&path) {
            Ok(_) => {
                self.pending_action = Some(RepoDialogAction::Open(path));
                self.hide();
            }
            Err(e) => {
                self.error_message = Some(format!("Not a git repository: {}", e));
            }
        }
    }

    /// Compute dialog bounds centered in screen
    fn dialog_bounds(&self, screen: Rect, scale: f32) -> Rect {
        let dialog_w = (400.0 * scale).min(screen.width * 0.8);
        let dialog_h = (180.0 * scale).min(screen.height * 0.5);
        let dialog_x = screen.x + (screen.width - dialog_w) / 2.0;
        let dialog_y = screen.y + (screen.height - dialog_h) / 2.0;
        Rect::new(dialog_x, dialog_y, dialog_w, dialog_h)
    }
}

impl Widget for RepoDialog {
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

        // Input field bounds
        let input_y = dialog.y + 40.0 * scale;
        let input_bounds = Rect::new(
            dialog.x + padding,
            input_y,
            dialog.width - padding * 2.0,
            line_h,
        );

        // Button bounds
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let button_gap = 8.0 * scale;
        let cancel_x = dialog.right() - padding - button_w;
        let open_x = cancel_x - button_w - button_gap;
        let open_bounds = Rect::new(open_x, button_y, button_w, line_h);
        let cancel_bounds = Rect::new(cancel_x, button_y, button_w, line_h);

        // Handle keyboard shortcuts
        if let InputEvent::KeyDown { key, .. } = event {
            match key {
                Key::Escape => {
                    self.pending_action = Some(RepoDialogAction::Cancel);
                    self.hide();
                    return EventResponse::Consumed;
                }
                Key::Enter => {
                    self.try_open();
                    return EventResponse::Consumed;
                }
                _ => {}
            }
        }

        // Route to text input
        if self.path_input.handle_event(event, input_bounds).is_consumed() {
            self.error_message = None; // Clear error on edit
            return EventResponse::Consumed;
        }

        // Route to buttons
        if self.open_button.handle_event(event, open_bounds).is_consumed() {
            if self.open_button.was_clicked() {
                self.try_open();
            }
            return EventResponse::Consumed;
        }

        if self.cancel_button.handle_event(event, cancel_bounds).is_consumed() {
            if self.cancel_button.was_clicked() {
                self.pending_action = Some(RepoDialogAction::Cancel);
                self.hide();
            }
            return EventResponse::Consumed;
        }

        // Click outside dialog dismisses
        if let InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } = event
            && !dialog.contains(*x, *y) {
                self.pending_action = Some(RepoDialogAction::Cancel);
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
        let line_height = text_renderer.line_height();

        // Semi-transparent backdrop (strong darkening for modal focus)
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            [0.0, 0.0, 0.0, 0.8],
        ));

        // Drop shadow (slightly larger, darker rect behind dialog for depth)
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

        // Dialog background (brighter than SURFACE_RAISED for contrast against dark backdrop)
        output.spline_vertices.extend(create_rect_vertices(
            &dialog,
            theme::SURFACE_RAISED.lighten(0.06).to_array(),
        ));

        // Dialog border (2px for visibility)
        output.spline_vertices.extend(create_rect_outline_vertices(
            &dialog,
            theme::BORDER_LIGHT.lighten(0.05).to_array(),
            2.0,
        ));

        // Title
        let title_y = dialog.y + padding;
        output.text_vertices.extend(text_renderer.layout_text(
            "Open Repository",
            dialog.x + padding,
            title_y,
            theme::TEXT_BRIGHT.to_array(),
        ));

        // Input field
        let input_y = dialog.y + 40.0 * scale;
        let input_bounds = Rect::new(
            dialog.x + padding,
            input_y,
            dialog.width - padding * 2.0,
            line_h,
        );
        output.extend(self.path_input.layout(text_renderer, input_bounds));

        // Error message
        if let Some(ref err) = self.error_message {
            let err_y = input_y + line_h + 4.0 * scale;
            output.text_vertices.extend(text_renderer.layout_text(
                err,
                dialog.x + padding,
                err_y,
                theme::STATUS_DIRTY.to_array(),
            ));
        }

        // Buttons at bottom
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let button_gap = 8.0 * scale;
        let cancel_x = dialog.right() - padding - button_w;
        let open_x = cancel_x - button_w - button_gap;

        let open_bounds = Rect::new(open_x, button_y, button_w, line_h);
        let cancel_bounds = Rect::new(cancel_x, button_y, button_w, line_h);

        output.extend(self.open_button.layout(text_renderer, open_bounds));
        output.extend(self.cancel_button.layout(text_renderer, cancel_bounds));

        // Hint text
        let hint_y = button_y + (line_h - line_height) / 2.0;
        output.text_vertices.extend(text_renderer.layout_text(
            "Enter or Tab to complete",
            dialog.x + padding,
            hint_y,
            theme::TEXT_MUTED.to_array(),
        ));

        output
    }
}
