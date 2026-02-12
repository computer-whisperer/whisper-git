//! Branch name dialog - modal overlay for entering a branch name

use git2::Oid;

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{
    create_dialog_backdrop, theme, Widget, WidgetOutput,
};
use crate::ui::widgets::{Button, TextInput};
use crate::ui::{Rect, TextRenderer};

/// Actions from the branch name dialog
#[derive(Clone, Debug)]
pub enum BranchNameDialogAction {
    Create(String, Oid),
    /// Create a worktree with (name, source_ref)
    CreateWorktree(String, String),
    /// Rename a branch: (new_name, old_name)
    Rename(String, String),
    Cancel,
}

/// A modal dialog for entering a new branch or tag name
pub struct BranchNameDialog {
    visible: bool,
    name_input: TextInput,
    create_button: Button,
    cancel_button: Button,
    target_oid: Option<Oid>,
    pending_action: Option<BranchNameDialogAction>,
    /// Title shown in the dialog (e.g. "Create Branch" or "Create Tag")
    title: String,
    /// When set, dialog is in worktree mode: stores the source ref (branch name or SHA)
    worktree_source: Option<String>,
    /// When set, dialog is in rename mode: stores the old branch name
    rename_source: Option<String>,
}

impl BranchNameDialog {
    pub fn new() -> Self {
        Self {
            visible: false,
            name_input: TextInput::new().with_placeholder("branch-name"),
            create_button: Button::new("Create").primary(),
            cancel_button: Button::new("Cancel"),
            target_oid: None,
            pending_action: None,
            title: "Create Branch".to_string(),
            worktree_source: None,
            rename_source: None,
        }
    }

    pub fn show(&mut self, default_name: &str, oid: Oid) {
        self.show_with_title("Create Branch", default_name, oid);
    }

    pub fn show_with_title(&mut self, title: &str, default_name: &str, oid: Oid) {
        self.visible = true;
        self.title = title.to_string();
        self.name_input.set_text(default_name);
        self.name_input.set_focused(true);
        self.target_oid = Some(oid);
        self.worktree_source = None;
        self.rename_source = None;
        self.pending_action = None;
    }

    /// Show the dialog in worktree creation mode.
    /// `default_name` is pre-filled, `source_ref` is the branch or SHA to base the worktree on.
    pub fn show_for_worktree(&mut self, default_name: &str, source_ref: &str) {
        self.visible = true;
        self.title = "Create Worktree".to_string();
        self.name_input = TextInput::new().with_placeholder("worktree-name");
        self.name_input.set_text(default_name);
        self.name_input.set_focused(true);
        self.target_oid = None;
        self.worktree_source = Some(source_ref.to_string());
        self.rename_source = None;
        self.pending_action = None;
    }

    /// Show the dialog in branch rename mode.
    /// `old_name` is the branch being renamed, pre-filled as the default text.
    pub fn show_for_rename(&mut self, old_name: &str) {
        self.visible = true;
        self.title = "Rename Branch".to_string();
        self.name_input = TextInput::new().with_placeholder("new-branch-name");
        self.name_input.set_text(old_name);
        self.name_input.set_focused(true);
        self.target_oid = None;
        self.worktree_source = None;
        self.rename_source = Some(old_name.to_string());
        self.pending_action = None;
        self.create_button = Button::new("Rename").primary();
    }

    /// Returns the current dialog title (e.g. "Create Branch" or "Create Tag")
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.name_input.set_focused(false);
        self.target_oid = None;
        self.worktree_source = None;
        self.rename_source = None;
        self.create_button = Button::new("Create").primary();
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn take_action(&mut self) -> Option<BranchNameDialogAction> {
        self.pending_action.take()
    }

    fn try_create(&mut self) {
        let name = self.name_input.text().trim().to_string();
        if name.is_empty() {
            return;
        }
        if let Some(ref old_name) = self.rename_source {
            self.pending_action = Some(BranchNameDialogAction::Rename(name, old_name.clone()));
            self.hide();
        } else if let Some(ref source) = self.worktree_source {
            self.pending_action = Some(BranchNameDialogAction::CreateWorktree(name, source.clone()));
            self.hide();
        } else if let Some(oid) = self.target_oid {
            self.pending_action = Some(BranchNameDialogAction::Create(name, oid));
            self.hide();
        }
    }

    /// Compute dialog bounds centered in screen
    fn dialog_bounds(&self, screen: Rect, scale: f32) -> Rect {
        let dialog_w = (380.0 * scale).min(screen.width * 0.8);
        let dialog_h = (160.0 * scale).min(screen.height * 0.5);
        let dialog_x = screen.x + (screen.width - dialog_w) / 2.0;
        let dialog_y = screen.y + (screen.height - dialog_h) / 2.0;
        Rect::new(dialog_x, dialog_y, dialog_w, dialog_h)
    }
}

impl Widget for BranchNameDialog {
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
        let create_x = cancel_x - button_w - button_gap;
        let create_bounds = Rect::new(create_x, button_y, button_w, line_h);
        let cancel_bounds = Rect::new(cancel_x, button_y, button_w, line_h);

        // Handle keyboard shortcuts first (before text input consumes them)
        if let InputEvent::KeyDown { key, .. } = event {
            match key {
                Key::Escape => {
                    self.pending_action = Some(BranchNameDialogAction::Cancel);
                    self.hide();
                    return EventResponse::Consumed;
                }
                Key::Enter => {
                    self.try_create();
                    return EventResponse::Consumed;
                }
                _ => {}
            }
        }

        // Route to text input
        if self.name_input.handle_event(event, input_bounds).is_consumed() {
            return EventResponse::Consumed;
        }

        // Route to buttons
        if self.create_button.handle_event(event, create_bounds).is_consumed() {
            if self.create_button.was_clicked() {
                self.try_create();
            }
            return EventResponse::Consumed;
        }

        if self.cancel_button.handle_event(event, cancel_bounds).is_consumed() {
            if self.cancel_button.was_clicked() {
                self.pending_action = Some(BranchNameDialogAction::Cancel);
                self.hide();
            }
            return EventResponse::Consumed;
        }

        // Click outside dialog dismisses (cancel)
        if let InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } = event
            && !dialog.contains(*x, *y) {
                self.pending_action = Some(BranchNameDialogAction::Cancel);
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

        // Input field
        let input_y = dialog.y + 40.0 * scale;
        let input_bounds = Rect::new(
            dialog.x + padding,
            input_y,
            dialog.width - padding * 2.0,
            line_h,
        );
        output.extend(self.name_input.layout(text_renderer, input_bounds));

        // Buttons at bottom
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let button_gap = 8.0 * scale;
        let cancel_x = dialog.right() - padding - button_w;
        let create_x = cancel_x - button_w - button_gap;

        let create_bounds = Rect::new(create_x, button_y, button_w, line_h);
        let cancel_bounds = Rect::new(cancel_x, button_y, button_w, line_h);

        output.extend(self.create_button.layout(text_renderer, create_bounds));
        output.extend(self.cancel_button.layout(text_renderer, cancel_bounds));

        output
    }
}
