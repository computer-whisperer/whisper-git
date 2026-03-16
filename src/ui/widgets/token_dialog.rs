//! Token management dialog - secure entry/removal of GitHub and GitLab API tokens

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{
    Widget, WidgetOutput, create_dialog_backdrop, create_rect_outline_vertices,
    create_rect_vertices, create_rounded_rect_vertices, theme,
};
use crate::ui::widgets::{Button, TextInput};
use crate::ui::{Rect, TextRenderer};

/// Actions emitted by the token dialog
#[derive(Clone, Debug)]
pub enum TokenDialogAction {
    Close,
    /// Set the GitHub token (may be empty to clear)
    SetGitHubToken(String),
    /// Set a GitLab token for a host
    SetGitLabToken { host: String, token: String },
    /// Remove a GitLab token entry
    RemoveGitLabToken(String),
}

/// Which token is being edited right now (inline editing mode)
#[derive(Clone, Debug, PartialEq, Eq)]
enum EditingTarget {
    GitHub,
    GitLab(usize), // index into gitlab_hosts
    NewGitLab,
}

/// Manage Tokens dialog
pub struct TokenDialog {
    visible: bool,
    close_button: Button,
    pending_actions: Vec<TokenDialogAction>,

    // --- GitHub ---
    github_has_token: bool,
    github_set_button: Button,
    github_clear_button: Button,

    // --- GitLab ---
    /// List of (host, has_token) pairs loaded from the token store
    gitlab_entries: Vec<(String, bool)>,
    /// Per-entry set/clear buttons
    gitlab_set_buttons: Vec<Button>,
    gitlab_clear_buttons: Vec<Button>,
    /// "Add host" button
    gitlab_add_button: Button,

    // --- Inline editing ---
    editing: Option<EditingTarget>,
    edit_input: TextInput,
    /// For new GitLab entries, the host input
    host_input: TextInput,
    edit_save_button: Button,
    edit_cancel_button: Button,
}

impl TokenDialog {
    pub fn new() -> Self {
        Self {
            visible: false,
            close_button: Button::new("Close"),
            pending_actions: Vec::new(),
            github_has_token: false,
            github_set_button: Button::new("Set"),
            github_clear_button: Button::new("Clear"),
            gitlab_entries: Vec::new(),
            gitlab_set_buttons: Vec::new(),
            gitlab_clear_buttons: Vec::new(),
            gitlab_add_button: Button::new("+ Add GitLab Host"),
            editing: None,
            edit_input: TextInput::new().with_placeholder("Paste token here"),
            host_input: TextInput::new().with_placeholder("gitlab.example.com"),
            edit_save_button: Button::new("Save").primary(),
            edit_cancel_button: Button::new("Cancel"),
        }
    }

    /// Show the dialog, loading current token state from the keychain.
    pub fn show(&mut self, github_has_token: bool, gitlab_hosts: Vec<(String, bool)>) {
        self.visible = true;
        self.github_has_token = github_has_token;
        self.gitlab_entries = gitlab_hosts;
        self.sync_gitlab_buttons();
        self.editing = None;
        self.edit_input.set_text("");
        self.host_input.set_text("");
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.editing = None;
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn take_actions(&mut self) -> Vec<TokenDialogAction> {
        std::mem::take(&mut self.pending_actions)
    }

    /// Ensure we have enough buttons for the gitlab entries
    fn sync_gitlab_buttons(&mut self) {
        while self.gitlab_set_buttons.len() < self.gitlab_entries.len() {
            self.gitlab_set_buttons.push(Button::new("Set"));
            self.gitlab_clear_buttons.push(Button::new("Clear"));
        }
    }

    fn dialog_bounds(&self, screen: Rect, scale: f32) -> Rect {
        let dialog_w = (500.0 * scale).min(screen.width * 0.85);
        // Height grows with number of gitlab entries
        let base_h = 260.0 * scale; // title + github + gitlab header + add button + close
        let entry_h = 44.0 * scale;
        let gitlab_h = entry_h * self.gitlab_entries.len() as f32;
        let edit_h = if self.editing.is_some() {
            80.0 * scale
        } else {
            0.0
        };
        let dialog_h = (base_h + gitlab_h + edit_h).min(screen.height * 0.85);
        let dialog_x = screen.x + (screen.width - dialog_w) / 2.0;
        let dialog_y = screen.y + (screen.height - dialog_h) / 2.0;
        Rect::new(dialog_x, dialog_y, dialog_w, dialog_h)
    }

    /// Begin editing a token
    fn start_editing(&mut self, target: EditingTarget) {
        self.editing = Some(target.clone());
        self.edit_input.set_text("");
        self.edit_input.set_focused(true);
        if target == EditingTarget::NewGitLab {
            self.host_input.set_text("");
            self.host_input.set_focused(true);
            self.edit_input.set_focused(false);
        }
    }

    /// Commit the current edit
    fn commit_edit(&mut self) {
        let Some(target) = self.editing.take() else {
            return;
        };
        let token = self.edit_input.text.trim().to_string();

        match target {
            EditingTarget::GitHub => {
                if !token.is_empty() {
                    self.pending_actions
                        .push(TokenDialogAction::SetGitHubToken(token));
                    self.github_has_token = true;
                }
            }
            EditingTarget::GitLab(idx) => {
                if !token.is_empty() {
                    if let Some((host, has)) = self.gitlab_entries.get_mut(idx) {
                        self.pending_actions.push(TokenDialogAction::SetGitLabToken {
                            host: host.clone(),
                            token,
                        });
                        *has = true;
                    }
                }
            }
            EditingTarget::NewGitLab => {
                let host = self.host_input.text.trim().to_string();
                if !host.is_empty() && !token.is_empty() {
                    self.pending_actions.push(TokenDialogAction::SetGitLabToken {
                        host: host.clone(),
                        token,
                    });
                    self.gitlab_entries.push((host, true));
                    self.sync_gitlab_buttons();
                }
            }
        }

        self.edit_input.set_text("");
        self.edit_input.set_focused(false);
        self.host_input.set_text("");
        self.host_input.set_focused(false);
    }

    fn cancel_edit(&mut self) {
        self.editing = None;
        self.edit_input.set_text("");
        self.edit_input.set_focused(false);
        self.host_input.set_text("");
        self.host_input.set_focused(false);
    }
}

impl Widget for TokenDialog {
    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        if !self.visible {
            return EventResponse::Ignored;
        }

        let scale = (bounds.height / 720.0).max(1.0);
        let dialog = self.dialog_bounds(bounds, scale);
        let padding = 16.0 * scale;
        let line_h = 32.0 * scale;
        let entry_h = 44.0 * scale;
        let title_h = 40.0 * scale;
        let btn_w = 70.0 * scale;
        let right_edge = dialog.right() - padding;
        let github_row_y = dialog.y + title_h + padding;
        let gitlab_header_y = github_row_y + entry_h + 8.0 * scale;
        let first_gitlab_y = gitlab_header_y + line_h + 4.0 * scale;

        // Escape closes (or cancels edit)
        if let InputEvent::KeyDown { key, .. } = event
            && *key == Key::Escape
        {
            if self.editing.is_some() {
                self.cancel_edit();
            } else {
                self.pending_actions.push(TokenDialogAction::Close);
                self.hide();
            }
            return EventResponse::Consumed;
        }

        // Enter commits edit
        if let InputEvent::KeyDown { key, .. } = event
            && *key == Key::Enter
            && self.editing.is_some()
        {
            self.commit_edit();
            return EventResponse::Consumed;
        }

        // --- Inline edit area (route text input events first) ---
        if self.editing.is_some() {
            // Compute edit area bounds
            let edit_area_y = self.edit_area_y(dialog, scale);
            let input_y = if self.editing == Some(EditingTarget::NewGitLab) {
                edit_area_y + line_h + 4.0 * scale // second row for token
            } else {
                edit_area_y
            };
            let input_bounds = Rect::new(
                dialog.x + padding,
                input_y,
                dialog.width - padding * 2.0 - btn_w * 2.0 - 16.0 * scale,
                line_h,
            );

            // For new GitLab, handle host input first
            if self.editing == Some(EditingTarget::NewGitLab) {
                let host_bounds = Rect::new(
                    dialog.x + padding,
                    edit_area_y,
                    dialog.width - padding * 2.0,
                    line_h,
                );

                // Click to focus host vs token
                if let InputEvent::MouseDown { x, y, .. } = event {
                    if host_bounds.contains(*x, *y) {
                        self.host_input.set_focused(true);
                        self.edit_input.set_focused(false);
                    } else if input_bounds.contains(*x, *y) {
                        self.host_input.set_focused(false);
                        self.edit_input.set_focused(true);
                    }
                }

                // Tab to switch focus
                if let InputEvent::KeyDown { key, .. } = event
                    && *key == Key::Tab
                {
                    if self.host_input.is_focused() {
                        self.host_input.set_focused(false);
                        self.edit_input.set_focused(true);
                    } else {
                        self.edit_input.set_focused(false);
                        self.host_input.set_focused(true);
                    }
                    return EventResponse::Consumed;
                }

                if self.host_input.is_focused() {
                    let r = self.host_input.handle_event(event, host_bounds);
                    if r.is_consumed() {
                        return r;
                    }
                }
            }

            if self.edit_input.is_focused()
                && self
                    .edit_input
                    .handle_event(event, input_bounds)
                    .is_consumed()
            {
                return EventResponse::Consumed;
            }

            // Save/Cancel buttons
            let btns_x = input_bounds.right() + 8.0 * scale;
            let save_bounds = Rect::new(btns_x, input_y, btn_w, line_h);
            let cancel_bounds = Rect::new(btns_x + btn_w + 4.0 * scale, input_y, btn_w, line_h);

            if self
                .edit_save_button
                .handle_event(event, save_bounds)
                .is_consumed()
            {
                if self.edit_save_button.was_clicked() {
                    self.commit_edit();
                }
                return EventResponse::Consumed;
            }
            if self
                .edit_cancel_button
                .handle_event(event, cancel_bounds)
                .is_consumed()
            {
                if self.edit_cancel_button.was_clicked() {
                    self.cancel_edit();
                }
                return EventResponse::Consumed;
            }
        }

        // --- GitHub row ---
        let github_set_bounds =
            Rect::new(right_edge - btn_w * 2.0 - 4.0 * scale, github_row_y, btn_w, line_h);
        let github_clear_bounds = Rect::new(right_edge - btn_w, github_row_y, btn_w, line_h);

        if self
            .github_set_button
            .handle_event(event, github_set_bounds)
            .is_consumed()
        {
            if self.github_set_button.was_clicked() {
                self.start_editing(EditingTarget::GitHub);
            }
            return EventResponse::Consumed;
        }
        if self.github_has_token
            && self
                .github_clear_button
                .handle_event(event, github_clear_bounds)
                .is_consumed()
        {
            if self.github_clear_button.was_clicked() {
                self.pending_actions
                    .push(TokenDialogAction::SetGitHubToken(String::new()));
                self.github_has_token = false;
            }
            return EventResponse::Consumed;
        }

        // --- GitLab rows ---
        for (i, (host, has_token)) in self.gitlab_entries.iter().enumerate() {
            let row_y = first_gitlab_y + i as f32 * entry_h;
            let set_bounds =
                Rect::new(right_edge - btn_w * 2.0 - 4.0 * scale, row_y, btn_w, line_h);
            let clear_bounds = Rect::new(right_edge - btn_w, row_y, btn_w, line_h);

            if let Some(btn) = self.gitlab_set_buttons.get_mut(i) {
                if btn.handle_event(event, set_bounds).is_consumed() {
                    if btn.was_clicked() {
                        self.start_editing(EditingTarget::GitLab(i));
                    }
                    return EventResponse::Consumed;
                }
            }
            if *has_token {
                if let Some(btn) = self.gitlab_clear_buttons.get_mut(i) {
                    if btn.handle_event(event, clear_bounds).is_consumed() {
                        if btn.was_clicked() {
                            self.pending_actions
                                .push(TokenDialogAction::RemoveGitLabToken(host.clone()));
                        }
                        return EventResponse::Consumed;
                    }
                }
            }
        }

        // --- Add GitLab host button ---
        let add_y = first_gitlab_y + self.gitlab_entries.len() as f32 * entry_h + 4.0 * scale;
        let add_w = 180.0 * scale;
        let add_bounds = Rect::new(dialog.x + padding, add_y, add_w, line_h);
        if self
            .gitlab_add_button
            .handle_event(event, add_bounds)
            .is_consumed()
        {
            if self.gitlab_add_button.was_clicked() {
                self.start_editing(EditingTarget::NewGitLab);
            }
            return EventResponse::Consumed;
        }

        // --- Close button ---
        let close_y = dialog.bottom() - padding - line_h;
        let close_w = 80.0 * scale;
        let close_bounds = Rect::new(right_edge - close_w, close_y, close_w, line_h);
        if self
            .close_button
            .handle_event(event, close_bounds)
            .is_consumed()
        {
            if self.close_button.was_clicked() {
                self.pending_actions.push(TokenDialogAction::Close);
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
            self.pending_actions.push(TokenDialogAction::Close);
            self.hide();
            return EventResponse::Consumed;
        }

        // Route MouseMove to all buttons for hover tracking
        if matches!(event, InputEvent::MouseMove { .. }) {
            let github_set_bounds =
                Rect::new(right_edge - btn_w * 2.0 - 4.0 * scale, github_row_y, btn_w, line_h);
            let github_clear_bounds =
                Rect::new(right_edge - btn_w, github_row_y, btn_w, line_h);
            self.github_set_button
                .handle_event(event, github_set_bounds);
            self.github_clear_button
                .handle_event(event, github_clear_bounds);

            for (i, _) in self.gitlab_entries.iter().enumerate() {
                let row_y = first_gitlab_y + i as f32 * entry_h;
                let set_bounds =
                    Rect::new(right_edge - btn_w * 2.0 - 4.0 * scale, row_y, btn_w, line_h);
                let clear_bounds = Rect::new(right_edge - btn_w, row_y, btn_w, line_h);
                if let Some(btn) = self.gitlab_set_buttons.get_mut(i) {
                    btn.handle_event(event, set_bounds);
                }
                if let Some(btn) = self.gitlab_clear_buttons.get_mut(i) {
                    btn.handle_event(event, clear_bounds);
                }
            }

            let add_y =
                first_gitlab_y + self.gitlab_entries.len() as f32 * entry_h + 4.0 * scale;
            let add_w = 180.0 * scale;
            let add_bounds = Rect::new(dialog.x + padding, add_y, add_w, line_h);
            self.gitlab_add_button.handle_event(event, add_bounds);

            let close_y = dialog.bottom() - padding - line_h;
            let close_w = 80.0 * scale;
            let close_bounds = Rect::new(right_edge - close_w, close_y, close_w, line_h);
            self.close_button.handle_event(event, close_bounds);

            if self.editing.is_some() {
                let edit_y = self.edit_area_y(dialog, scale);
                let input_w = dialog.width - padding * 2.0 - btn_w * 2.0 - 16.0 * scale;
                let btn_y = if self.editing == Some(EditingTarget::NewGitLab) {
                    edit_y + line_h + 4.0 * scale
                } else {
                    edit_y
                };
                let btns_x = dialog.x + padding + input_w + 8.0 * scale;
                self.edit_save_button
                    .handle_event(event, Rect::new(btns_x, btn_y, btn_w, line_h));
                self.edit_cancel_button.handle_event(
                    event,
                    Rect::new(btns_x + btn_w + 4.0 * scale, btn_y, btn_w, line_h),
                );
            }
        }

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
        let entry_h = 44.0 * scale;
        let title_h = 40.0 * scale;
        let line_height = text_renderer.line_height();
        let btn_w = 70.0 * scale;
        let right_edge = dialog.right() - padding;

        // Backdrop + dialog background
        create_dialog_backdrop(&mut output, &bounds, &dialog, scale);

        // Title
        output.text_vertices.extend(text_renderer.layout_text(
            "Manage Tokens",
            dialog.x + padding,
            dialog.y + padding,
            theme::TEXT_BRIGHT.to_array(),
        ));

        // Title separator
        let sep_y = dialog.y + 36.0 * scale;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(dialog.x + padding, sep_y, dialog.width - padding * 2.0, 1.0),
            theme::BORDER.with_alpha(0.4).to_array(),
        ));

        // === GitHub section ===
        let github_row_y = dialog.y + title_h + padding;
        let label_y = github_row_y + (line_h - line_height) / 2.0;
        output.text_vertices.extend(text_renderer.layout_text(
            "GitHub",
            dialog.x + padding,
            label_y,
            theme::TEXT.to_array(),
        ));

        // Status indicator
        let status_text = if self.github_has_token {
            "\u{2022} configured"
        } else {
            "\u{2022} not set"
        };
        let status_color = if self.github_has_token {
            [0.34, 0.80, 0.44, 1.0] // green
        } else {
            theme::TEXT_MUTED.to_array()
        };
        let status_x = dialog.x + padding + text_renderer.measure_text("GitHub") + 12.0 * scale;
        output.text_vertices.extend(
            text_renderer.layout_text(status_text, status_x, label_y, status_color),
        );

        // Set / Clear buttons
        let set_bounds =
            Rect::new(right_edge - btn_w * 2.0 - 4.0 * scale, github_row_y, btn_w, line_h);
        output.extend(self.github_set_button.layout(text_renderer, set_bounds));
        if self.github_has_token {
            let clear_bounds = Rect::new(right_edge - btn_w, github_row_y, btn_w, line_h);
            output.extend(
                self.github_clear_button
                    .layout(text_renderer, clear_bounds),
            );
        }

        // GitHub separator
        let gh_sep_y = github_row_y + entry_h;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(
                dialog.x + padding,
                gh_sep_y,
                dialog.width - padding * 2.0,
                1.0,
            ),
            theme::BORDER.to_array(),
        ));

        // === GitLab section header ===
        let gitlab_header_y = github_row_y + entry_h + 8.0 * scale;
        let gl_label_y = gitlab_header_y + (line_h - line_height) / 2.0;
        output.text_vertices.extend(text_renderer.layout_text(
            "GitLab",
            dialog.x + padding,
            gl_label_y,
            theme::TEXT.to_array(),
        ));

        // GitLab entries
        let first_gitlab_y = gitlab_header_y + line_h + 4.0 * scale;
        for (i, (host, has_token)) in self.gitlab_entries.iter().enumerate() {
            let row_y = first_gitlab_y + i as f32 * entry_h;
            let row_label_y = row_y + (line_h - line_height) / 2.0;

            // Host name
            output.text_vertices.extend(text_renderer.layout_text(
                host,
                dialog.x + padding + 12.0 * scale,
                row_label_y,
                theme::TEXT_MUTED.to_array(),
            ));

            // Status
            let gl_status = if *has_token {
                "\u{2022} configured"
            } else {
                "\u{2022} not set"
            };
            let gl_status_color = if *has_token {
                [0.34, 0.80, 0.44, 1.0]
            } else {
                theme::TEXT_MUTED.to_array()
            };
            let gl_status_x = dialog.x
                + padding
                + 12.0 * scale
                + text_renderer.measure_text(host)
                + 12.0 * scale;
            output.text_vertices.extend(text_renderer.layout_text(
                gl_status,
                gl_status_x,
                row_label_y,
                gl_status_color,
            ));

            // Set / Clear buttons
            let set_bounds =
                Rect::new(right_edge - btn_w * 2.0 - 4.0 * scale, row_y, btn_w, line_h);
            if let Some(btn) = self.gitlab_set_buttons.get(i) {
                output.extend(btn.layout(text_renderer, set_bounds));
            }
            if *has_token {
                let clear_bounds = Rect::new(right_edge - btn_w, row_y, btn_w, line_h);
                if let Some(btn) = self.gitlab_clear_buttons.get(i) {
                    output.extend(btn.layout(text_renderer, clear_bounds));
                }
            }

            // Row separator
            let row_sep_y = row_y + entry_h - 4.0 * scale;
            output.spline_vertices.extend(create_rect_vertices(
                &Rect::new(
                    dialog.x + padding + 12.0 * scale,
                    row_sep_y,
                    dialog.width - padding * 2.0 - 12.0 * scale,
                    1.0,
                ),
                theme::BORDER.with_alpha(0.3).to_array(),
            ));
        }

        // Add GitLab host button
        let add_y = first_gitlab_y + self.gitlab_entries.len() as f32 * entry_h + 4.0 * scale;
        let add_w = 180.0 * scale;
        let add_bounds = Rect::new(dialog.x + padding, add_y, add_w, line_h);
        output.extend(self.gitlab_add_button.layout(text_renderer, add_bounds));

        // === Inline edit area ===
        if let Some(ref target) = self.editing {
            let edit_y = self.edit_area_y(dialog, scale);

            // Edit area background
            let edit_area_h = if *target == EditingTarget::NewGitLab {
                line_h * 2.0 + 12.0 * scale
            } else {
                line_h + 8.0 * scale
            };
            let edit_bg = Rect::new(
                dialog.x + padding * 0.5,
                edit_y - 4.0 * scale,
                dialog.width - padding,
                edit_area_h,
            );
            output
                .spline_vertices
                .extend(create_rounded_rect_vertices(
                    &edit_bg,
                    theme::SURFACE.with_alpha(0.9).to_array(),
                    4.0,
                ));
            output
                .spline_vertices
                .extend(create_rect_outline_vertices(
                    &edit_bg,
                    theme::ACCENT.with_alpha(0.5).to_array(),
                    1.0,
                ));

            let input_w = dialog.width - padding * 2.0 - btn_w * 2.0 - 16.0 * scale;

            if *target == EditingTarget::NewGitLab {
                // Host input on first line
                let host_bounds =
                    Rect::new(dialog.x + padding, edit_y, input_w + btn_w * 2.0 + 8.0 * scale, line_h);
                output.extend(self.host_input.layout(text_renderer, host_bounds));

                // Token input on second line
                let token_y = edit_y + line_h + 4.0 * scale;
                let token_bounds = Rect::new(dialog.x + padding, token_y, input_w, line_h);
                output.extend(self.edit_input.layout(text_renderer, token_bounds));

                // Save/Cancel on second line
                let btns_x = dialog.x + padding + input_w + 8.0 * scale;
                let save_bounds = Rect::new(btns_x, token_y, btn_w, line_h);
                let cancel_bounds =
                    Rect::new(btns_x + btn_w + 4.0 * scale, token_y, btn_w, line_h);
                output.extend(self.edit_save_button.layout(text_renderer, save_bounds));
                output.extend(
                    self.edit_cancel_button
                        .layout(text_renderer, cancel_bounds),
                );
            } else {
                // Token input + save/cancel on one line
                let token_bounds = Rect::new(dialog.x + padding, edit_y, input_w, line_h);
                output.extend(self.edit_input.layout(text_renderer, token_bounds));

                let btns_x = dialog.x + padding + input_w + 8.0 * scale;
                let save_bounds = Rect::new(btns_x, edit_y, btn_w, line_h);
                let cancel_bounds =
                    Rect::new(btns_x + btn_w + 4.0 * scale, edit_y, btn_w, line_h);
                output.extend(self.edit_save_button.layout(text_renderer, save_bounds));
                output.extend(
                    self.edit_cancel_button
                        .layout(text_renderer, cancel_bounds),
                );
            }
        }

        // === Close button ===
        let close_y = dialog.bottom() - padding - line_h;
        let close_w = 80.0 * scale;
        let close_bounds = Rect::new(right_edge - close_w, close_y, close_w, line_h);

        // Button separator
        let btn_sep_y = close_y - 8.0 * scale;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(
                dialog.x + padding,
                btn_sep_y,
                dialog.width - padding * 2.0,
                1.0,
            ),
            theme::BORDER.with_alpha(0.4).to_array(),
        ));

        output.extend(self.close_button.layout(text_renderer, close_bounds));

        // Keychain status note at bottom-left
        let note_y = close_y + (line_h - line_height) / 2.0;
        let note = if crate::token_store::is_available() {
            "Stored in system keychain"
        } else {
            "Keychain unavailable \u{2014} stored in config"
        };
        output.text_vertices.extend(text_renderer.layout_text(
            note,
            dialog.x + padding,
            note_y,
            theme::TEXT_MUTED.to_array(),
        ));

        output
    }
}

impl TokenDialog {
    /// Y position for the inline edit area (below the relevant row)
    fn edit_area_y(&self, dialog: Rect, scale: f32) -> f32 {
        let padding = 16.0 * scale;
        let title_h = 40.0 * scale;
        let entry_h = 44.0 * scale;
        let line_h = 32.0 * scale;
        let github_row_y = dialog.y + title_h + padding;

        match self.editing {
            Some(EditingTarget::GitHub) => github_row_y + entry_h,
            Some(EditingTarget::GitLab(idx)) => {
                let gitlab_header_y = github_row_y + entry_h + 8.0 * scale;
                let first_gitlab_y = gitlab_header_y + line_h + 4.0 * scale;
                first_gitlab_y + (idx as f32 + 1.0) * entry_h
            }
            Some(EditingTarget::NewGitLab) => {
                let gitlab_header_y = github_row_y + entry_h + 8.0 * scale;
                let first_gitlab_y = gitlab_header_y + line_h + 4.0 * scale;
                let add_y =
                    first_gitlab_y + self.gitlab_entries.len() as f32 * entry_h + 4.0 * scale;
                add_y + line_h + 4.0 * scale
            }
            None => dialog.bottom(), // shouldn't happen
        }
    }

}
