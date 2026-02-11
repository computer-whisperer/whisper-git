//! Remote dialog - modal overlay for adding/editing git remotes

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{
    create_rect_vertices, create_rounded_rect_vertices, theme, Widget, WidgetId, WidgetOutput, WidgetState,
};
use crate::ui::widgets::{Button, TextInput};
use crate::ui::{Rect, TextRenderer};

/// The mode the remote dialog was opened in
#[derive(Clone, Debug)]
pub enum RemoteDialogMode {
    /// Adding a new remote (name + URL)
    Add,
    /// Editing the URL of an existing remote
    EditUrl(String),
    /// Renaming an existing remote
    Rename(String),
}

/// Actions from the remote dialog
#[derive(Clone, Debug)]
pub enum RemoteDialogAction {
    /// Add a new remote with (name, url)
    AddRemote(String, String),
    /// Edit the URL of a remote: (remote_name, new_url)
    EditUrl(String, String),
    /// Rename a remote: (old_name, new_name)
    Rename(String, String),
    /// User cancelled
    Cancel,
}

/// A modal dialog for managing git remotes
#[allow(dead_code)]
pub struct RemoteDialog {
    id: WidgetId,
    state: WidgetState,
    visible: bool,
    mode: RemoteDialogMode,
    /// First input field (name for Add/Rename, URL for EditUrl)
    first_input: TextInput,
    /// Second input field (URL for Add mode, unused otherwise)
    second_input: TextInput,
    confirm_button: Button,
    cancel_button: Button,
    pending_action: Option<RemoteDialogAction>,
    title: String,
}

impl RemoteDialog {
    pub fn new() -> Self {
        Self {
            id: WidgetId::new(),
            state: WidgetState::new(),
            visible: false,
            mode: RemoteDialogMode::Add,
            first_input: TextInput::new().with_placeholder("remote-name"),
            second_input: TextInput::new().with_placeholder("https://github.com/user/repo.git"),
            confirm_button: Button::new("Add").primary(),
            cancel_button: Button::new("Cancel"),
            pending_action: None,
            title: "Add Remote".to_string(),
        }
    }

    /// Show dialog in Add mode (two fields: name + URL)
    pub fn show_add(&mut self) {
        self.visible = true;
        self.mode = RemoteDialogMode::Add;
        self.title = "Add Remote".to_string();
        self.first_input = TextInput::new().with_placeholder("remote-name");
        self.first_input.set_text("origin");
        self.first_input.set_focused(true);
        self.second_input = TextInput::new().with_placeholder("https://github.com/user/repo.git");
        self.second_input.set_text("");
        self.second_input.set_focused(false);
        self.confirm_button = Button::new("Add").primary();
        self.pending_action = None;
    }

    /// Show dialog in Edit URL mode (one field: URL, pre-filled)
    pub fn show_edit_url(&mut self, remote_name: &str, current_url: &str) {
        self.visible = true;
        self.mode = RemoteDialogMode::EditUrl(remote_name.to_string());
        self.title = format!("Edit URL - {}", remote_name);
        self.first_input = TextInput::new().with_placeholder("https://github.com/user/repo.git");
        self.first_input.set_text(current_url);
        self.first_input.set_focused(true);
        self.confirm_button = Button::new("Save").primary();
        self.pending_action = None;
    }

    /// Show dialog in Rename mode (one field: new name, pre-filled)
    pub fn show_rename(&mut self, remote_name: &str) {
        self.visible = true;
        self.mode = RemoteDialogMode::Rename(remote_name.to_string());
        self.title = format!("Rename Remote - {}", remote_name);
        self.first_input = TextInput::new().with_placeholder("new-remote-name");
        self.first_input.set_text(remote_name);
        self.first_input.set_focused(true);
        self.confirm_button = Button::new("Rename").primary();
        self.pending_action = None;
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.first_input.set_focused(false);
        self.second_input.set_focused(false);
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn take_action(&mut self) -> Option<RemoteDialogAction> {
        self.pending_action.take()
    }

    fn try_confirm(&mut self) {
        match &self.mode {
            RemoteDialogMode::Add => {
                let name = self.first_input.text().trim().to_string();
                let url = self.second_input.text().trim().to_string();
                if name.is_empty() || url.is_empty() {
                    return;
                }
                self.pending_action = Some(RemoteDialogAction::AddRemote(name, url));
                self.hide();
            }
            RemoteDialogMode::EditUrl(remote_name) => {
                let url = self.first_input.text().trim().to_string();
                if url.is_empty() {
                    return;
                }
                self.pending_action = Some(RemoteDialogAction::EditUrl(remote_name.clone(), url));
                self.hide();
            }
            RemoteDialogMode::Rename(old_name) => {
                let new_name = self.first_input.text().trim().to_string();
                if new_name.is_empty() {
                    return;
                }
                self.pending_action = Some(RemoteDialogAction::Rename(old_name.clone(), new_name));
                self.hide();
            }
        }
    }

    /// Whether we're showing two input fields (Add mode)
    fn is_two_field(&self) -> bool {
        matches!(self.mode, RemoteDialogMode::Add)
    }

    /// Compute dialog bounds centered in screen
    fn dialog_bounds(&self, screen: Rect, scale: f32) -> Rect {
        let dialog_w = (420.0 * scale).min(screen.width * 0.85);
        let dialog_h = if self.is_two_field() {
            (220.0 * scale).min(screen.height * 0.5)
        } else {
            (160.0 * scale).min(screen.height * 0.5)
        };
        let dialog_x = screen.x + (screen.width - dialog_w) / 2.0;
        let dialog_y = screen.y + (screen.height - dialog_h) / 2.0;
        Rect::new(dialog_x, dialog_y, dialog_w, dialog_h)
    }
}

impl Widget for RemoteDialog {
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
        let label_h = 18.0 * scale;

        // First input field bounds
        let first_label_y = dialog.y + 40.0 * scale;
        let first_input_y = first_label_y + label_h;
        let input_w = dialog.width - padding * 2.0;
        let first_input_bounds = Rect::new(
            dialog.x + padding,
            first_input_y,
            input_w,
            line_h,
        );

        // Second input field bounds (only in Add mode)
        let second_input_bounds = if self.is_two_field() {
            let second_label_y = first_input_y + line_h + 8.0 * scale;
            let second_input_y = second_label_y + label_h;
            Some(Rect::new(
                dialog.x + padding,
                second_input_y,
                input_w,
                line_h,
            ))
        } else {
            None
        };

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
                    self.pending_action = Some(RemoteDialogAction::Cancel);
                    self.hide();
                    return EventResponse::Consumed;
                }
                Key::Enter => {
                    self.try_confirm();
                    return EventResponse::Consumed;
                }
                Key::Tab => {
                    // Tab between fields in Add mode
                    if self.is_two_field() {
                        if self.first_input.is_focused() {
                            self.first_input.set_focused(false);
                            self.second_input.set_focused(true);
                        } else {
                            self.second_input.set_focused(false);
                            self.first_input.set_focused(true);
                        }
                        return EventResponse::Consumed;
                    }
                }
                _ => {}
            }
        }

        // Handle click-to-focus between fields
        if let InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } = event {
            if first_input_bounds.contains(*x, *y) {
                self.first_input.set_focused(true);
                self.second_input.set_focused(false);
            } else if let Some(ref sb) = second_input_bounds {
                if sb.contains(*x, *y) {
                    self.first_input.set_focused(false);
                    self.second_input.set_focused(true);
                }
            }
        }

        // Route to first input
        if self.first_input.handle_event(event, first_input_bounds).is_consumed() {
            return EventResponse::Consumed;
        }

        // Route to second input (Add mode)
        if let Some(sb) = second_input_bounds {
            if self.second_input.handle_event(event, sb).is_consumed() {
                return EventResponse::Consumed;
            }
        }

        // Route to buttons
        if self.confirm_button.handle_event(event, confirm_bounds).is_consumed() {
            if self.confirm_button.was_clicked() {
                self.try_confirm();
            }
            return EventResponse::Consumed;
        }

        if self.cancel_button.handle_event(event, cancel_bounds).is_consumed() {
            if self.cancel_button.was_clicked() {
                self.pending_action = Some(RemoteDialogAction::Cancel);
                self.hide();
            }
            return EventResponse::Consumed;
        }

        // Click outside dialog dismisses (cancel)
        if let InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } = event
            && !dialog.contains(*x, *y) {
                self.pending_action = Some(RemoteDialogAction::Cancel);
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

        // Semi-transparent backdrop
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            [0.0, 0.0, 0.0, 0.8],
        ));

        let corner_radius = 8.0 * scale;

        // Drop shadow
        let shadow_offset = 3.0 * scale;
        let shadow_rect = Rect::new(
            dialog.x + shadow_offset,
            dialog.y + shadow_offset,
            dialog.width,
            dialog.height,
        );
        output.spline_vertices.extend(create_rounded_rect_vertices(
            &shadow_rect,
            [0.0, 0.0, 0.0, 0.5],
            corner_radius,
        ));

        // Dialog background
        output.spline_vertices.extend(create_rounded_rect_vertices(
            &dialog,
            theme::SURFACE_RAISED.lighten(0.06).to_array(),
            corner_radius,
        ));

        // Title
        let title_y = dialog.y + padding;
        output.text_vertices.extend(text_renderer.layout_text(
            &self.title,
            dialog.x + padding,
            title_y,
            theme::TEXT_BRIGHT.to_array(),
        ));

        // First label + input
        let first_label_y = dialog.y + 40.0 * scale;
        let first_label = match &self.mode {
            RemoteDialogMode::Add => "Name",
            RemoteDialogMode::EditUrl(_) => "URL",
            RemoteDialogMode::Rename(_) => "New Name",
        };
        output.text_vertices.extend(text_renderer.layout_text(
            first_label,
            dialog.x + padding,
            first_label_y,
            theme::TEXT_MUTED.to_array(),
        ));

        let first_input_y = first_label_y + label_h;
        let input_w = dialog.width - padding * 2.0;
        let first_input_bounds = Rect::new(
            dialog.x + padding,
            first_input_y,
            input_w,
            line_h,
        );
        output.extend(self.first_input.layout(text_renderer, first_input_bounds));

        // Second label + input (Add mode only)
        if self.is_two_field() {
            let second_label_y = first_input_y + line_h + 8.0 * scale;
            output.text_vertices.extend(text_renderer.layout_text(
                "URL",
                dialog.x + padding,
                second_label_y,
                theme::TEXT_MUTED.to_array(),
            ));

            let second_input_y = second_label_y + label_h;
            let second_input_bounds = Rect::new(
                dialog.x + padding,
                second_input_y,
                input_w,
                line_h,
            );
            output.extend(self.second_input.layout(text_renderer, second_input_bounds));
        }

        // Buttons at bottom
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let button_gap = 8.0 * scale;
        let cancel_x = dialog.right() - padding - button_w;
        let confirm_x = cancel_x - button_w - button_gap;

        let confirm_bounds = Rect::new(confirm_x, button_y, button_w, line_h);
        let cancel_bounds = Rect::new(cancel_x, button_y, button_w, line_h);

        output.extend(self.confirm_button.layout(text_renderer, confirm_bounds));
        output.extend(self.cancel_button.layout(text_renderer, cancel_bounds));

        output
    }
}
