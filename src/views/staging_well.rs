//! Staging well view - commit message editor and file staging

use crate::git::WorkingDirStatus;
use crate::input::{EventResponse, InputEvent, Key};
use crate::ui::widget::{create_rect_vertices, create_rect_outline_vertices, theme, WidgetOutput};
use crate::ui::widgets::{Button, FileList, FileListAction, TextArea, TextInput};
use crate::ui::{Rect, TextRenderer, Widget};

/// Actions from the staging well
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StagingAction {
    StageFile(String),
    UnstageFile(String),
    StageAll,
    UnstageAll,
    Commit(String),
    ViewDiff(String),
}

/// The staging well view containing commit message and file lists
pub struct StagingWell {
    /// Commit subject line input
    pub subject_input: TextInput,
    /// Commit body text area
    pub body_area: TextArea,
    /// Staged files list
    pub staged_list: FileList,
    /// Unstaged files list
    pub unstaged_list: FileList,
    /// Stage all button
    stage_all_btn: Button,
    /// Unstage all button
    unstage_all_btn: Button,
    /// Commit button
    commit_btn: Button,
    /// Pending action
    pending_action: Option<StagingAction>,
    /// Which section has focus (0=subject, 1=body, 2=staged, 3=unstaged)
    focus_section: usize,
    /// Display scale factor for 4K/HiDPI scaling
    pub scale: f32,
}

impl StagingWell {
    pub fn new() -> Self {
        Self {
            subject_input: TextInput::new()
                .with_placeholder("Commit subject line...")
                .with_max_length(72),
            body_area: TextArea::new(),
            staged_list: FileList::new("Staged", true),
            unstaged_list: FileList::new("Unstaged", false),
            stage_all_btn: Button::new("Stage All"),
            unstage_all_btn: Button::new("Unstage All"),
            commit_btn: Button::new("Commit").primary(),
            pending_action: None,
            focus_section: 0,
            scale: 1.0,
        }
    }

    /// Update from working directory status
    pub fn update_status(&mut self, status: &WorkingDirStatus) {
        use crate::ui::widgets::file_list::FileEntry;

        let staged: Vec<FileEntry> = status.staged.iter().map(FileEntry::from).collect();
        let unstaged: Vec<FileEntry> = status.unstaged.iter().map(FileEntry::from).collect();

        self.staged_list.set_files(staged);
        self.unstaged_list.set_files(unstaged);
    }

    /// Get and clear any pending action
    pub fn take_action(&mut self) -> Option<StagingAction> {
        self.pending_action.take()
    }

    /// Get the commit message (subject + body)
    pub fn commit_message(&self) -> String {
        let subject = self.subject_input.text();
        let body = self.body_area.text();

        if body.trim().is_empty() {
            subject.to_string()
        } else {
            format!("{}\n\n{}", subject, body)
        }
    }

    /// Clear the commit message
    pub fn clear_message(&mut self) {
        self.subject_input.set_text("");
        self.body_area.set_text("");
    }

    /// Check if commit button should be enabled
    pub fn can_commit(&self) -> bool {
        !self.subject_input.text().is_empty() && !self.staged_list.files.is_empty()
    }

    fn cycle_focus(&mut self, forward: bool) {
        let sections = 4;
        if forward {
            self.focus_section = (self.focus_section + 1) % sections;
        } else {
            self.focus_section = (self.focus_section + sections - 1) % sections;
        }
        self.update_focus_state();
    }

    fn update_focus_state(&mut self) {
        self.subject_input.set_focused(self.focus_section == 0);
        self.body_area.set_focused(self.focus_section == 1);
        self.staged_list.set_focused(self.focus_section == 2);
        self.unstaged_list.set_focused(self.focus_section == 3);
    }

    /// Sync button styles based on current state. Call before layout.
    pub fn update_button_state(&mut self) {
        if self.can_commit() {
            self.commit_btn.background = theme::ACCENT;
            self.commit_btn.hover_background = crate::ui::Color::rgba(0.35, 0.70, 1.0, 1.0);
            self.commit_btn.pressed_background = crate::ui::Color::rgba(0.20, 0.55, 0.85, 1.0);
            self.commit_btn.text_color = theme::TEXT_BRIGHT;
            self.commit_btn.border_color = None;
        } else {
            self.commit_btn.background = theme::SURFACE_RAISED;
            self.commit_btn.hover_background = theme::SURFACE_HOVER;
            self.commit_btn.pressed_background = theme::SURFACE;
            self.commit_btn.text_color = theme::TEXT;
            self.commit_btn.border_color = Some(theme::BORDER);
        }
    }

    /// Update hover state for child widgets based on mouse position
    pub fn update_hover(&mut self, x: f32, y: f32, bounds: Rect) {
        let (_, _, staged_bounds, unstaged_bounds, buttons_bounds) =
            self.compute_regions(bounds);

        self.staged_list.update_hover(x, y, staged_bounds);
        self.unstaged_list.update_hover(x, y, unstaged_bounds);
        self.stage_all_btn.update_hover(x, y, self.stage_all_button_bounds(buttons_bounds));
        self.unstage_all_btn.update_hover(x, y, self.unstage_all_button_bounds(buttons_bounds));
        self.commit_btn.update_hover(x, y, self.commit_button_bounds(buttons_bounds));
    }

    /// Handle input events
    pub fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        // Calculate sub-regions
        let (subject_bounds, body_bounds, staged_bounds, unstaged_bounds, buttons_bounds) =
            self.compute_regions(bounds);

        // Tab to cycle focus within staging sections.
        // When cycling past the last section, return Ignored so Tab bubbles up
        // to the panel-level cycling in main.rs.
        if let InputEvent::KeyDown { key: Key::Tab, modifiers, .. } = event {
            let forward = !modifiers.shift;
            let sections = 4;
            if forward && self.focus_section == sections - 1 {
                // Would wrap past last section - let it bubble up
                return EventResponse::Ignored;
            } else if !forward && self.focus_section == 0 {
                // Would wrap past first section - let it bubble up
                return EventResponse::Ignored;
            }
            self.cycle_focus(forward);
            return EventResponse::Consumed;
        }

        // Ctrl+Enter to commit
        if let InputEvent::KeyDown { key: Key::Enter, modifiers, .. } = event {
            if modifiers.only_ctrl() && self.can_commit() {
                self.pending_action = Some(StagingAction::Commit(self.commit_message()));
                return EventResponse::Consumed;
            }
        }

        // Handle button clicks first (they sit outside the focus sections)
        if self.stage_all_btn.handle_event(event, self.stage_all_button_bounds(buttons_bounds)).is_consumed() {
            if self.stage_all_btn.was_clicked() {
                self.pending_action = Some(StagingAction::StageAll);
            }
            return EventResponse::Consumed;
        }

        if self.unstage_all_btn.handle_event(event, self.unstage_all_button_bounds(buttons_bounds)).is_consumed() {
            if self.unstage_all_btn.was_clicked() {
                self.pending_action = Some(StagingAction::UnstageAll);
            }
            return EventResponse::Consumed;
        }

        if self.commit_btn.handle_event(event, self.commit_button_bounds(buttons_bounds)).is_consumed() {
            if self.commit_btn.was_clicked() && self.can_commit() {
                self.pending_action = Some(StagingAction::Commit(self.commit_message()));
            }
            return EventResponse::Consumed;
        }

        // For MouseDown, determine focus section before routing
        if let InputEvent::MouseDown { x, y, .. } = event {
            if subject_bounds.contains(*x, *y) {
                self.focus_section = 0;
                self.update_focus_state();
            } else if body_bounds.contains(*x, *y) {
                self.focus_section = 1;
                self.update_focus_state();
            } else if staged_bounds.contains(*x, *y) {
                self.focus_section = 2;
                self.update_focus_state();
            } else if unstaged_bounds.contains(*x, *y) {
                self.focus_section = 3;
                self.update_focus_state();
            }
        }

        // Route to focused section
        match self.focus_section {
            0 => {
                let response = self.subject_input.handle_event(event, subject_bounds);
                if response.is_consumed() {
                    return response;
                }
            }
            1 => {
                let response = self.body_area.handle_event(event, body_bounds);
                if response.is_consumed() {
                    return response;
                }
            }
            2 => {
                let response = self.staged_list.handle_event(event, staged_bounds);
                if response.is_consumed() {
                    if let Some(action) = self.staged_list.take_action() {
                        match action {
                            FileListAction::ToggleStage(path) => {
                                self.pending_action = Some(StagingAction::UnstageFile(path));
                            }
                            FileListAction::ViewDiff(path) => {
                                self.pending_action = Some(StagingAction::ViewDiff(path));
                            }
                            FileListAction::UnstageAll => {
                                self.pending_action = Some(StagingAction::UnstageAll);
                            }
                            _ => {}
                        }
                    }
                    return response;
                }
            }
            3 => {
                let response = self.unstaged_list.handle_event(event, unstaged_bounds);
                if response.is_consumed() {
                    if let Some(action) = self.unstaged_list.take_action() {
                        match action {
                            FileListAction::ToggleStage(path) => {
                                self.pending_action = Some(StagingAction::StageFile(path));
                            }
                            FileListAction::ViewDiff(path) => {
                                self.pending_action = Some(StagingAction::ViewDiff(path));
                            }
                            FileListAction::StageAll => {
                                self.pending_action = Some(StagingAction::StageAll);
                            }
                            _ => {}
                        }
                    }
                    return response;
                }
            }
            _ => {}
        }

        EventResponse::Ignored
    }

    fn compute_regions(&self, bounds: Rect) -> (Rect, Rect, Rect, Rect, Rect) {
        let s = self.scale;
        let padding = 8.0 * s;
        let inner = bounds.inset(padding);

        // Reserve space for "Commit Message" title row
        let title_height = 22.0 * s;
        let (_title_row, remaining) = inner.take_top(title_height);

        // Subject line: single line
        let subject_height = 32.0 * s;
        let (subject, remaining) = remaining.take_top(subject_height);

        // Gap
        let (_, remaining) = remaining.take_top(padding);

        // Body area
        let body_height = 80.0 * s;
        let (body, remaining) = remaining.take_top(body_height);

        // Gap
        let (_, remaining) = remaining.take_top(padding);

        // Split remaining between staged and unstaged lists
        let button_area_height = 40.0 * s;
        let list_area_height = remaining.height - button_area_height;
        let (lists_area, buttons) = remaining.take_top(list_area_height);

        // Split lists area
        let (staged, unstaged) = lists_area.split_vertical(0.5);
        let staged = staged.pad(0.0, 0.0, 0.0, padding / 2.0);
        let unstaged = unstaged.pad(0.0, padding / 2.0, 0.0, 0.0);

        (subject, body, staged, unstaged, buttons)
    }

    fn stage_all_button_bounds(&self, buttons: Rect) -> Rect {
        let s = self.scale;
        Rect::new(buttons.x, buttons.y + 8.0 * s, 130.0 * s, 28.0 * s)
    }

    fn unstage_all_button_bounds(&self, buttons: Rect) -> Rect {
        let s = self.scale;
        Rect::new(buttons.x + 138.0 * s, buttons.y + 8.0 * s, 150.0 * s, 28.0 * s)
    }

    fn commit_button_bounds(&self, buttons: Rect) -> Rect {
        let s = self.scale;
        Rect::new(buttons.right() - 110.0 * s, buttons.y + 8.0 * s, 110.0 * s, 28.0 * s)
    }

    /// Layout the staging well
    pub fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        // Background - elevated surface for panel
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            theme::SURFACE.to_array(),
        ));

        // Border
        output.spline_vertices.extend(create_rect_outline_vertices(
            &bounds,
            theme::BORDER.to_array(),
            1.0,
        ));

        let (subject_bounds, body_bounds, staged_bounds, unstaged_bounds, buttons_bounds) =
            self.compute_regions(bounds);

        // Title (in the reserved title row above the subject input)
        let s = self.scale;
        let title_y = bounds.y + 10.0 * s;
        output.text_vertices.extend(text_renderer.layout_text(
            "Commit Message",
            bounds.x + 10.0 * s,
            title_y,
            theme::TEXT.to_array(),
        ));

        // Character count for subject
        let char_count = format!("{}/72", self.subject_input.text().len());
        output.text_vertices.extend(text_renderer.layout_text(
            &char_count,
            bounds.right() - text_renderer.measure_text(&char_count) - 10.0 * s,
            title_y,
            theme::TEXT_MUTED.to_array(),
        ));

        // Subject input
        output.extend(self.subject_input.layout(text_renderer, subject_bounds));

        // Body area
        output.extend(self.body_area.layout(text_renderer, body_bounds));

        // Staged files list
        output.extend(self.staged_list.layout(text_renderer, staged_bounds));

        // Unstaged files list
        output.extend(self.unstaged_list.layout(text_renderer, unstaged_bounds));

        // Buttons
        output.extend(self.stage_all_btn.layout(text_renderer, self.stage_all_button_bounds(buttons_bounds)));
        output.extend(self.unstage_all_btn.layout(text_renderer, self.unstage_all_button_bounds(buttons_bounds)));

        // Commit button (uses stored instance to preserve hover state)
        let commit_bounds = self.commit_button_bounds(buttons_bounds);
        output.extend(self.commit_btn.layout(text_renderer, commit_bounds));

        output
    }
}

impl Default for StagingWell {
    fn default() -> Self {
        Self::new()
    }
}
