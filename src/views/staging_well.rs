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

    /// Handle input events
    pub fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        // Calculate sub-regions
        let (subject_bounds, body_bounds, staged_bounds, unstaged_bounds, buttons_bounds) =
            self.compute_regions(bounds);

        // Tab to cycle focus
        if let InputEvent::KeyDown { key: Key::Tab, modifiers } = event {
            self.cycle_focus(!modifiers.shift);
            return EventResponse::Consumed;
        }

        // Ctrl+Enter to commit
        if let InputEvent::KeyDown { key: Key::Enter, modifiers } = event {
            if modifiers.only_ctrl() && self.can_commit() {
                self.pending_action = Some(StagingAction::Commit(self.commit_message()));
                return EventResponse::Consumed;
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
                    // Check for file list actions
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
                    // Check for file list actions
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

        // Handle button clicks
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

        // Click to focus
        if let InputEvent::MouseDown { x, y, .. } = event {
            if subject_bounds.contains(*x, *y) {
                self.focus_section = 0;
                self.update_focus_state();
                return self.subject_input.handle_event(event, subject_bounds);
            } else if body_bounds.contains(*x, *y) {
                self.focus_section = 1;
                self.update_focus_state();
                return self.body_area.handle_event(event, body_bounds);
            } else if staged_bounds.contains(*x, *y) {
                self.focus_section = 2;
                self.update_focus_state();
                return self.staged_list.handle_event(event, staged_bounds);
            } else if unstaged_bounds.contains(*x, *y) {
                self.focus_section = 3;
                self.update_focus_state();
                return self.unstaged_list.handle_event(event, unstaged_bounds);
            }
        }

        EventResponse::Ignored
    }

    fn compute_regions(&self, bounds: Rect) -> (Rect, Rect, Rect, Rect, Rect) {
        let padding = 8.0;
        let inner = bounds.inset(padding);

        // Subject line: single line (~32px)
        let subject_height = 32.0;
        let (subject, remaining) = inner.take_top(subject_height);

        // Gap
        let (_, remaining) = remaining.take_top(padding);

        // Body area: ~80px
        let body_height = 80.0;
        let (body, remaining) = remaining.take_top(body_height);

        // Gap
        let (_, remaining) = remaining.take_top(padding);

        // Split remaining between staged and unstaged lists
        let list_area_height = remaining.height - 40.0; // Leave room for buttons
        let (lists_area, buttons) = remaining.take_top(list_area_height);

        // Split lists area
        let (staged, unstaged) = lists_area.split_vertical(0.5);
        let staged = staged.pad(0.0, 0.0, 0.0, padding / 2.0);
        let unstaged = unstaged.pad(0.0, padding / 2.0, 0.0, 0.0);

        (subject, body, staged, unstaged, buttons)
    }

    fn stage_all_button_bounds(&self, buttons: Rect) -> Rect {
        Rect::new(buttons.x, buttons.y + 8.0, 130.0, 28.0)
    }

    fn unstage_all_button_bounds(&self, buttons: Rect) -> Rect {
        Rect::new(buttons.x + 138.0, buttons.y + 8.0, 150.0, 28.0)
    }

    fn commit_button_bounds(&self, buttons: Rect) -> Rect {
        Rect::new(buttons.right() - 110.0, buttons.y + 8.0, 110.0, 28.0)
    }

    /// Layout the staging well
    pub fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        // Background
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            theme::BACKGROUND.to_array(),
        ));

        // Border
        output.spline_vertices.extend(create_rect_outline_vertices(
            &bounds,
            theme::BORDER.to_array(),
            1.0,
        ));

        let (subject_bounds, body_bounds, staged_bounds, unstaged_bounds, buttons_bounds) =
            self.compute_regions(bounds);

        // Title
        let title_y = bounds.y + 4.0;
        output.text_vertices.extend(text_renderer.layout_text(
            "Commit Message",
            bounds.x + 8.0,
            title_y,
            theme::TEXT.to_array(),
        ));

        // Character count for subject
        let char_count = format!("{}/72", self.subject_input.text().len());
        output.text_vertices.extend(text_renderer.layout_text(
            &char_count,
            bounds.right() - text_renderer.measure_text(&char_count) - 8.0,
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

        // Commit button (with visual state based on whether commit is possible)
        let commit_bounds = self.commit_button_bounds(buttons_bounds);
        if self.can_commit() {
            let btn = Button::new("Commit").primary();
            output.extend(btn.layout(text_renderer, commit_bounds));
        } else {
            let btn = Button::new("Commit");
            output.extend(btn.layout(text_renderer, commit_bounds));
        }

        output
    }
}

impl Default for StagingWell {
    fn default() -> Self {
        Self::new()
    }
}
