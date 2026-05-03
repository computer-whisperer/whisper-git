//! Token management dialog - secure entry/removal of GitHub and GitLab API tokens

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::Rect;
use crate::ui::widget::{
    LayoutCtx, Widget, WidgetOutput, create_dialog_backdrop, create_rect_outline_vertices,
    create_rect_vertices, create_rounded_rect_vertices, theme,
};
use crate::ui::widgets::{Button, TextInput};

/// Actions emitted by the token dialog
#[derive(Clone, Debug)]
pub enum TokenDialogAction {
    Close,
    /// Set the GitHub token (may be empty to clear)
    SetGitHubToken(String),
    /// Set a GitLab token for a host
    SetGitLabToken {
        host: String,
        token: String,
    },
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

/// Pre-computed layout positions for the token dialog.
/// Both handle_event and layout use this to stay in sync.
struct DialogLayout {
    scale: f32,
    dialog: Rect,
    padding: f32,
    line_h: f32,
    entry_h: f32,
    btn_w: f32,
    right_edge: f32,
    github_row_y: f32,
    /// Y positions for each GitLab entry row
    gitlab_row_ys: Vec<f32>,
    /// Y position for the "Add GitLab Host" button
    add_button_y: f32,
    /// Edit area Y (if editing), and whether it's a two-line (NewGitLab) edit
    edit_area: Option<(f32, bool)>,
    /// Y position for the close button (anchored to dialog bottom)
    close_y: f32,
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

    /// Compute the dialog bounds and all element positions.
    fn compute_layout(&self, screen: Rect) -> DialogLayout {
        let scale = (screen.height / 720.0).max(1.0);
        let padding = 16.0 * scale;
        let line_h = 32.0 * scale;
        let entry_h = 44.0 * scale;
        let title_h = 40.0 * scale;
        let btn_w = 70.0 * scale;

        // Compute edit area height
        let edit_h = match &self.editing {
            Some(EditingTarget::NewGitLab) => line_h * 2.0 + 20.0 * scale, // two-line edit
            Some(_) => line_h + 16.0 * scale,                              // one-line edit
            None => 0.0,
        };

        // Dialog bounds
        let dialog_w = (500.0 * scale).min(screen.width * 0.85);
        let base_h = 260.0 * scale;
        let gitlab_h = entry_h * self.gitlab_entries.len() as f32;
        let dialog_h = (base_h + gitlab_h + edit_h).min(screen.height * 0.85);
        let dialog_x = screen.x + (screen.width - dialog_w) / 2.0;
        let dialog_y = screen.y + (screen.height - dialog_h) / 2.0;
        let dialog = Rect::new(dialog_x, dialog_y, dialog_w, dialog_h);
        let right_edge = dialog.right() - padding;

        // Use a running Y cursor to position elements
        let mut y = dialog.y + title_h + padding;
        let github_row_y = y;
        y += entry_h + 8.0 * scale; // github row + separator gap

        // Insert edit area after GitHub if editing GitHub
        let mut edit_area = None;
        if self.editing == Some(EditingTarget::GitHub) {
            edit_area = Some((y, false));
            y += edit_h;
        }

        // GitLab header
        y += line_h + 4.0 * scale; // "GitLab" label + gap

        // GitLab entries
        let mut gitlab_row_ys = Vec::with_capacity(self.gitlab_entries.len());
        for i in 0..self.gitlab_entries.len() {
            gitlab_row_ys.push(y);
            y += entry_h;

            // Insert edit area after this GitLab entry if editing it
            if self.editing == Some(EditingTarget::GitLab(i)) {
                edit_area = Some((y, false));
                y += edit_h;
            }
        }

        // Add button
        let add_button_y = y + 4.0 * scale;
        y = add_button_y + line_h + 4.0 * scale;

        // Insert edit area after add button if adding new GitLab host
        if self.editing == Some(EditingTarget::NewGitLab) {
            edit_area = Some((y, true));
        }

        // Close button anchored to bottom
        let close_y = dialog.bottom() - padding - line_h;

        DialogLayout {
            scale,
            dialog,
            padding,
            line_h,
            entry_h,
            btn_w,
            right_edge,
            github_row_y,
            gitlab_row_ys,
            add_button_y,
            edit_area,
            close_y,
        }
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
                if !token.is_empty()
                    && let Some((host, has)) = self.gitlab_entries.get_mut(idx)
                {
                    self.pending_actions
                        .push(TokenDialogAction::SetGitLabToken {
                            host: host.clone(),
                            token,
                        });
                    *has = true;
                }
            }
            EditingTarget::NewGitLab => {
                let host = self.host_input.text.trim().to_string();
                if !host.is_empty() && !token.is_empty() {
                    self.pending_actions
                        .push(TokenDialogAction::SetGitLabToken {
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

    /// Compute bounds for the edit input and save/cancel buttons.
    fn edit_input_bounds(
        &self,
        dl: &DialogLayout,
        edit_y: f32,
        two_line: bool,
    ) -> (Rect, Rect, Rect, Rect) {
        let input_w = dl.dialog.width - dl.padding * 2.0 - dl.btn_w * 2.0 - 16.0 * dl.scale;

        if two_line {
            // Host input on first line (full width)
            let host_bounds = Rect::new(
                dl.dialog.x + dl.padding,
                edit_y,
                dl.dialog.width - dl.padding * 2.0,
                dl.line_h,
            );
            // Token input + buttons on second line
            let token_y = edit_y + dl.line_h + 4.0 * dl.scale;
            let token_bounds = Rect::new(dl.dialog.x + dl.padding, token_y, input_w, dl.line_h);
            let btns_x = dl.dialog.x + dl.padding + input_w + 8.0 * dl.scale;
            let save_bounds = Rect::new(btns_x, token_y, dl.btn_w, dl.line_h);
            let cancel_bounds = Rect::new(
                btns_x + dl.btn_w + 4.0 * dl.scale,
                token_y,
                dl.btn_w,
                dl.line_h,
            );
            (host_bounds, token_bounds, save_bounds, cancel_bounds)
        } else {
            // Token input + buttons on one line
            let token_bounds = Rect::new(dl.dialog.x + dl.padding, edit_y, input_w, dl.line_h);
            let btns_x = dl.dialog.x + dl.padding + input_w + 8.0 * dl.scale;
            let save_bounds = Rect::new(btns_x, edit_y, dl.btn_w, dl.line_h);
            let cancel_bounds = Rect::new(
                btns_x + dl.btn_w + 4.0 * dl.scale,
                edit_y,
                dl.btn_w,
                dl.line_h,
            );
            // host_bounds unused for single-line — return zero rect
            (
                Rect::new(0.0, 0.0, 0.0, 0.0),
                token_bounds,
                save_bounds,
                cancel_bounds,
            )
        }
    }
}

impl Widget for TokenDialog {
    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        if !self.visible {
            return EventResponse::Ignored;
        }

        let dl = self.compute_layout(bounds);

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
        if let Some((edit_y, two_line)) = dl.edit_area {
            let (host_bounds, token_bounds, save_bounds, cancel_bounds) =
                self.edit_input_bounds(&dl, edit_y, two_line);

            // For new GitLab, handle host input
            if two_line {
                // Click to focus host vs token
                if let InputEvent::MouseDown { x, y, .. } = event {
                    if host_bounds.contains(*x, *y) {
                        self.host_input.set_focused(true);
                        self.edit_input.set_focused(false);
                    } else if token_bounds.contains(*x, *y) {
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

                if self.host_input.is_focused()
                    && self
                        .host_input
                        .handle_event(event, host_bounds)
                        .is_consumed()
                {
                    return EventResponse::Consumed;
                }
            }

            if self.edit_input.is_focused()
                && self
                    .edit_input
                    .handle_event(event, token_bounds)
                    .is_consumed()
            {
                return EventResponse::Consumed;
            }

            // Save/Cancel buttons
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
        let github_set_bounds = Rect::new(
            dl.right_edge - dl.btn_w * 2.0 - 4.0 * dl.scale,
            dl.github_row_y,
            dl.btn_w,
            dl.line_h,
        );
        let github_clear_bounds = Rect::new(
            dl.right_edge - dl.btn_w,
            dl.github_row_y,
            dl.btn_w,
            dl.line_h,
        );

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
            let row_y = dl.gitlab_row_ys[i];
            let set_bounds = Rect::new(
                dl.right_edge - dl.btn_w * 2.0 - 4.0 * dl.scale,
                row_y,
                dl.btn_w,
                dl.line_h,
            );
            let clear_bounds = Rect::new(dl.right_edge - dl.btn_w, row_y, dl.btn_w, dl.line_h);

            if let Some(btn) = self.gitlab_set_buttons.get_mut(i)
                && btn.handle_event(event, set_bounds).is_consumed()
            {
                if btn.was_clicked() {
                    self.start_editing(EditingTarget::GitLab(i));
                }
                return EventResponse::Consumed;
            }
            if *has_token
                && let Some(btn) = self.gitlab_clear_buttons.get_mut(i)
                && btn.handle_event(event, clear_bounds).is_consumed()
            {
                if btn.was_clicked() {
                    self.pending_actions
                        .push(TokenDialogAction::RemoveGitLabToken(host.clone()));
                }
                return EventResponse::Consumed;
            }
        }

        // --- Add GitLab host button ---
        let add_w = 180.0 * dl.scale;
        let add_bounds = Rect::new(dl.dialog.x + dl.padding, dl.add_button_y, add_w, dl.line_h);
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
        let close_w = 80.0 * dl.scale;
        let close_bounds = Rect::new(dl.right_edge - close_w, dl.close_y, close_w, dl.line_h);
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
            && !dl.dialog.contains(*x, *y)
        {
            self.pending_actions.push(TokenDialogAction::Close);
            self.hide();
            return EventResponse::Consumed;
        }

        // Route MouseMove to all buttons for hover tracking
        if matches!(event, InputEvent::MouseMove { .. }) {
            self.github_set_button
                .handle_event(event, github_set_bounds);
            self.github_clear_button
                .handle_event(event, github_clear_bounds);

            for (i, _) in self.gitlab_entries.iter().enumerate() {
                let row_y = dl.gitlab_row_ys[i];
                let set_bounds = Rect::new(
                    dl.right_edge - dl.btn_w * 2.0 - 4.0 * dl.scale,
                    row_y,
                    dl.btn_w,
                    dl.line_h,
                );
                let clear_bounds = Rect::new(dl.right_edge - dl.btn_w, row_y, dl.btn_w, dl.line_h);
                if let Some(btn) = self.gitlab_set_buttons.get_mut(i) {
                    btn.handle_event(event, set_bounds);
                }
                if let Some(btn) = self.gitlab_clear_buttons.get_mut(i) {
                    btn.handle_event(event, clear_bounds);
                }
            }

            self.gitlab_add_button.handle_event(event, add_bounds);
            self.close_button.handle_event(event, close_bounds);

            if let Some((edit_y, two_line)) = dl.edit_area {
                let (_, _, save_bounds, cancel_bounds) =
                    self.edit_input_bounds(&dl, edit_y, two_line);
                self.edit_save_button.handle_event(event, save_bounds);
                self.edit_cancel_button.handle_event(event, cancel_bounds);
            }
        }

        EventResponse::Consumed
    }

    fn layout(&mut self, ctx: &LayoutCtx, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();
        if !self.visible {
            return output;
        }

        let dl = self.compute_layout(bounds);
        let line_height = ctx.text.line_height();

        // Backdrop + dialog background
        create_dialog_backdrop(&mut output, &bounds, &dl.dialog, dl.scale);

        // Title
        output.text_vertices.extend(ctx.text.layout_text(
            "Manage Tokens",
            dl.dialog.x + dl.padding,
            dl.dialog.y + dl.padding,
            theme::TEXT_BRIGHT.to_array(),
        ));

        // Title separator
        let sep_y = dl.dialog.y + 36.0 * dl.scale;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(
                dl.dialog.x + dl.padding,
                sep_y,
                dl.dialog.width - dl.padding * 2.0,
                1.0,
            ),
            theme::BORDER.with_alpha(0.4).to_array(),
        ));

        // === GitHub section ===
        let label_y = dl.github_row_y + (dl.line_h - line_height) / 2.0;
        output.text_vertices.extend(ctx.text.layout_text(
            "GitHub",
            dl.dialog.x + dl.padding,
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
            [0.34, 0.80, 0.44, 1.0]
        } else {
            theme::TEXT_MUTED.to_array()
        };
        let status_x = dl.dialog.x + dl.padding + ctx.text.measure_text("GitHub") + 12.0 * dl.scale;
        output.text_vertices.extend(ctx.text.layout_text(
            status_text,
            status_x,
            label_y,
            status_color,
        ));

        // Set / Clear buttons
        let set_bounds = Rect::new(
            dl.right_edge - dl.btn_w * 2.0 - 4.0 * dl.scale,
            dl.github_row_y,
            dl.btn_w,
            dl.line_h,
        );
        output.extend(self.github_set_button.layout(ctx, set_bounds));
        if self.github_has_token {
            let clear_bounds = Rect::new(
                dl.right_edge - dl.btn_w,
                dl.github_row_y,
                dl.btn_w,
                dl.line_h,
            );
            output.extend(self.github_clear_button.layout(ctx, clear_bounds));
        }

        // GitHub separator
        let gh_sep_y = dl.github_row_y + dl.entry_h;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(
                dl.dialog.x + dl.padding,
                gh_sep_y,
                dl.dialog.width - dl.padding * 2.0,
                1.0,
            ),
            theme::BORDER.to_array(),
        ));

        // === GitLab section header ===
        // Header is between github separator and first gitlab row
        let gitlab_header_y = if !dl.gitlab_row_ys.is_empty() {
            dl.gitlab_row_ys[0] - dl.line_h - 4.0 * dl.scale
        } else {
            dl.add_button_y - dl.line_h - 4.0 * dl.scale
        };
        let gl_label_y = gitlab_header_y + (dl.line_h - line_height) / 2.0;
        output.text_vertices.extend(ctx.text.layout_text(
            "GitLab",
            dl.dialog.x + dl.padding,
            gl_label_y,
            theme::TEXT.to_array(),
        ));

        // GitLab entries
        for (i, (host, has_token)) in self.gitlab_entries.iter().enumerate() {
            let row_y = dl.gitlab_row_ys[i];
            let row_label_y = row_y + (dl.line_h - line_height) / 2.0;

            // Host name
            output.text_vertices.extend(ctx.text.layout_text(
                host,
                dl.dialog.x + dl.padding + 12.0 * dl.scale,
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
            let gl_status_x = dl.dialog.x
                + dl.padding
                + 12.0 * dl.scale
                + ctx.text.measure_text(host)
                + 12.0 * dl.scale;
            output.text_vertices.extend(ctx.text.layout_text(
                gl_status,
                gl_status_x,
                row_label_y,
                gl_status_color,
            ));

            // Set / Clear buttons
            let set_bounds = Rect::new(
                dl.right_edge - dl.btn_w * 2.0 - 4.0 * dl.scale,
                row_y,
                dl.btn_w,
                dl.line_h,
            );
            if let Some(btn) = self.gitlab_set_buttons.get_mut(i) {
                output.extend(btn.layout(ctx, set_bounds));
            }
            if *has_token {
                let clear_bounds = Rect::new(dl.right_edge - dl.btn_w, row_y, dl.btn_w, dl.line_h);
                if let Some(btn) = self.gitlab_clear_buttons.get_mut(i) {
                    output.extend(btn.layout(ctx, clear_bounds));
                }
            }

            // Row separator
            let row_sep_y = row_y + dl.entry_h - 4.0 * dl.scale;
            output.spline_vertices.extend(create_rect_vertices(
                &Rect::new(
                    dl.dialog.x + dl.padding + 12.0 * dl.scale,
                    row_sep_y,
                    dl.dialog.width - dl.padding * 2.0 - 12.0 * dl.scale,
                    1.0,
                ),
                theme::BORDER.with_alpha(0.3).to_array(),
            ));
        }

        // Add GitLab host button
        let add_w = 180.0 * dl.scale;
        let add_bounds = Rect::new(dl.dialog.x + dl.padding, dl.add_button_y, add_w, dl.line_h);
        output.extend(self.gitlab_add_button.layout(ctx, add_bounds));

        // === Inline edit area ===
        if let Some((edit_y, two_line)) = dl.edit_area {
            let (host_bounds, token_bounds, save_bounds, cancel_bounds) =
                self.edit_input_bounds(&dl, edit_y, two_line);

            // Edit area background
            let edit_area_h = if two_line {
                dl.line_h * 2.0 + 12.0 * dl.scale
            } else {
                dl.line_h + 8.0 * dl.scale
            };
            let edit_bg = Rect::new(
                dl.dialog.x + dl.padding * 0.5,
                edit_y - 4.0 * dl.scale,
                dl.dialog.width - dl.padding,
                edit_area_h,
            );
            output.spline_vertices.extend(create_rounded_rect_vertices(
                &edit_bg,
                theme::SURFACE.with_alpha(0.9).to_array(),
                4.0,
            ));
            output.spline_vertices.extend(create_rect_outline_vertices(
                &edit_bg,
                theme::ACCENT.with_alpha(0.5).to_array(),
                1.0,
            ));

            if two_line {
                output.extend(self.host_input.layout(ctx, host_bounds));
            }
            output.extend(self.edit_input.layout(ctx, token_bounds));
            output.extend(self.edit_save_button.layout(ctx, save_bounds));
            output.extend(self.edit_cancel_button.layout(ctx, cancel_bounds));
        }

        // === Close button ===
        let close_w = 80.0 * dl.scale;
        let close_bounds = Rect::new(dl.right_edge - close_w, dl.close_y, close_w, dl.line_h);

        // Button separator
        let btn_sep_y = dl.close_y - 8.0 * dl.scale;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(
                dl.dialog.x + dl.padding,
                btn_sep_y,
                dl.dialog.width - dl.padding * 2.0,
                1.0,
            ),
            theme::BORDER.with_alpha(0.4).to_array(),
        ));

        output.extend(self.close_button.layout(ctx, close_bounds));

        // Keychain status note at bottom-left
        let note_y = dl.close_y + (dl.line_h - line_height) / 2.0;
        let note = if crate::token_store::is_available() {
            "Stored in system keychain"
        } else {
            "Keychain unavailable \u{2014} stored in config"
        };
        output.text_vertices.extend(ctx.text.layout_text(
            note,
            dl.dialog.x + dl.padding,
            note_y,
            theme::TEXT_MUTED.to_array(),
        ));

        output
    }
}
