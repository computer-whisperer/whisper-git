//! File list widget for staging area

use crate::git::{FileStatus, FileStatusKind};
use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{create_rect_vertices, create_rect_outline_vertices, theme, Widget, WidgetId, WidgetOutput, WidgetState};
use crate::ui::{Rect, TextRenderer};

/// A file entry in the list
#[derive(Clone, Debug)]
pub struct FileEntry {
    pub path: String,
    pub status: FileStatusKind,
    pub additions: usize,
    pub deletions: usize,
}

impl From<&FileStatus> for FileEntry {
    fn from(status: &FileStatus) -> Self {
        Self {
            path: status.path.clone(),
            status: status.status,
            additions: 0,
            deletions: 0,
        }
    }
}

/// Actions from the file list
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileListAction {
    /// Toggle staging state of selected file
    ToggleStage(String),
    /// View diff of selected file
    ViewDiff(String),
    /// Stage all files
    StageAll,
    /// Unstage all files
    UnstageAll,
}

/// A scrollable list of files with status indicators
pub struct FileList {
    id: WidgetId,
    state: WidgetState,
    /// Title for the list
    pub title: String,
    /// Files in the list
    pub files: Vec<FileEntry>,
    /// Selected file index
    selected: Option<usize>,
    /// Scroll offset
    scroll_offset: usize,
    /// Whether these are staged files
    pub is_staged: bool,
    /// Pending action
    pending_action: Option<FileListAction>,
    /// Index of the item under the mouse cursor
    hovered_index: Option<usize>,
}

impl FileList {
    pub fn new(title: impl Into<String>, is_staged: bool) -> Self {
        Self {
            id: WidgetId::new(),
            state: WidgetState::new(),
            title: title.into(),
            files: Vec::new(),
            selected: None,
            scroll_offset: 0,
            is_staged,
            pending_action: None,
            hovered_index: None,
        }
    }

    /// Set the files to display
    pub fn set_files(&mut self, files: Vec<FileEntry>) {
        self.files = files;
        // Adjust selection if needed
        if let Some(idx) = self.selected {
            if idx >= self.files.len() {
                self.selected = if self.files.is_empty() { None } else { Some(self.files.len() - 1) };
            }
        }
    }

    /// Get the selected file path
    pub fn selected_file(&self) -> Option<&str> {
        self.selected.and_then(|idx| self.files.get(idx).map(|f| f.path.as_str()))
    }

    /// Check for pending action and clear it
    pub fn take_action(&mut self) -> Option<FileListAction> {
        self.pending_action.take()
    }

    /// Get total additions/deletions
    pub fn totals(&self) -> (usize, usize) {
        self.files.iter().fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions))
    }

    fn visible_lines(&self, bounds: &Rect) -> usize {
        let header_height = 28.0; // Match layout header height
        let line_height = 22.0;   // Slightly tighter line height
        ((bounds.height - header_height) / line_height).max(1.0) as usize
    }

    /// Update hover state based on mouse position
    pub fn update_hover(&mut self, x: f32, y: f32, bounds: Rect) {
        if !bounds.contains(x, y) {
            self.hovered_index = None;
            return;
        }

        let header_height = 28.0;
        let entry_height = 22.0;
        let content_y = bounds.y + header_height + 6.0;

        if y < content_y {
            self.hovered_index = None;
            return;
        }

        let hovered_line = ((y - content_y) / entry_height) as usize;
        let file_idx = self.scroll_offset + hovered_line;
        if file_idx < self.files.len() {
            self.hovered_index = Some(file_idx);
        } else {
            self.hovered_index = None;
        }
    }

    fn ensure_selection_visible(&mut self, bounds: &Rect) {
        if let Some(idx) = self.selected {
            let visible = self.visible_lines(bounds);
            if idx < self.scroll_offset {
                self.scroll_offset = idx;
            } else if idx >= self.scroll_offset + visible {
                self.scroll_offset = idx - visible + 1;
            }
        }
    }
}

impl Widget for FileList {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        match event {
            InputEvent::MouseDown {
                button: MouseButton::Left,
                x,
                y,
                ..
            } => {
                if bounds.contains(*x, *y) {
                    self.state.focused = true;

                    // Check if clicking on a file
                    let header_height = 24.0;
                    let line_height = 24.0;
                    let content_y = bounds.y + header_height;

                    if *y > content_y {
                        let clicked_line = ((*y - content_y) / line_height) as usize;
                        let file_idx = self.scroll_offset + clicked_line;
                        if file_idx < self.files.len() {
                            self.selected = Some(file_idx);
                            return EventResponse::Consumed;
                        }
                    }
                }
            }
            InputEvent::KeyDown { key, .. } if self.state.focused => {
                match key {
                    Key::J | Key::Down => {
                        if let Some(idx) = self.selected {
                            if idx + 1 < self.files.len() {
                                self.selected = Some(idx + 1);
                                self.ensure_selection_visible(&bounds);
                            }
                        } else if !self.files.is_empty() {
                            self.selected = Some(0);
                        }
                        return EventResponse::Consumed;
                    }
                    Key::K | Key::Up => {
                        if let Some(idx) = self.selected {
                            if idx > 0 {
                                self.selected = Some(idx - 1);
                                self.ensure_selection_visible(&bounds);
                            }
                        } else if !self.files.is_empty() {
                            self.selected = Some(0);
                        }
                        return EventResponse::Consumed;
                    }
                    Key::Space => {
                        // Toggle staging
                        if let Some(path) = self.selected_file() {
                            self.pending_action = Some(FileListAction::ToggleStage(path.to_string()));
                            return EventResponse::Consumed;
                        }
                    }
                    Key::Enter => {
                        // View diff
                        if let Some(path) = self.selected_file() {
                            self.pending_action = Some(FileListAction::ViewDiff(path.to_string()));
                            return EventResponse::Consumed;
                        }
                    }
                    Key::A => {
                        // Stage/unstage all
                        if self.is_staged {
                            self.pending_action = Some(FileListAction::UnstageAll);
                        } else {
                            self.pending_action = Some(FileListAction::StageAll);
                        }
                        return EventResponse::Consumed;
                    }
                    _ => {}
                }
            }
            InputEvent::Scroll { delta_y, .. } if bounds.contains(event.position().unwrap_or((0.0, 0.0)).0, event.position().unwrap_or((0.0, 0.0)).1) => {
                let scroll_lines = (-delta_y / 20.0) as i32;
                if scroll_lines < 0 {
                    self.scroll_offset = self.scroll_offset.saturating_sub((-scroll_lines) as usize);
                } else {
                    let max_scroll = self.files.len().saturating_sub(self.visible_lines(&bounds));
                    self.scroll_offset = (self.scroll_offset + scroll_lines as usize).min(max_scroll);
                }
                return EventResponse::Consumed;
            }
            _ => {}
        }

        EventResponse::Ignored
    }

    fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        // Background
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            theme::SURFACE.to_array(),
        ));

        // Border - accent color when focused, thicker
        let border_color = if self.state.focused {
            theme::ACCENT
        } else {
            theme::BORDER
        };
        let border_thickness = if self.state.focused { 2.0 } else { 1.0 };
        output.spline_vertices.extend(create_rect_outline_vertices(
            &bounds,
            border_color.to_array(),
            border_thickness,
        ));

        let line_height = text_renderer.line_height();
        let header_height = line_height + 12.0;

        // Header background - slightly elevated
        let header_rect = Rect::new(bounds.x + 1.0, bounds.y + 1.0, bounds.width - 2.0, header_height);
        output.spline_vertices.extend(create_rect_vertices(
            &header_rect,
            theme::SURFACE_RAISED.to_array(),
        ));

        // Header with title and file count
        let title_text = format!("{} ({} files)", self.title, self.files.len());
        output.text_vertices.extend(text_renderer.layout_text(
            &title_text,
            bounds.x + 10.0,
            bounds.y + 6.0,
            theme::TEXT_BRIGHT.to_array(),
        ));

        // Totals on the right side of header
        let (total_add, total_del) = self.totals();
        if total_add > 0 || total_del > 0 {
            let stats_text = format!("+{} -{}", total_add, total_del);
            let stats_x = bounds.right() - text_renderer.measure_text(&stats_text) - 10.0;
            output.text_vertices.extend(text_renderer.layout_text(
                &stats_text,
                stats_x,
                bounds.y + 6.0,
                theme::TEXT_MUTED.to_array(),
            ));
        }

        // Separator line
        let sep_y = bounds.y + header_height;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(bounds.x + 1.0, sep_y, bounds.width - 2.0, 1.0),
            theme::BORDER.to_array(),
        ));

        // File entries
        let content_y = sep_y + 6.0;
        let entry_height = 22.0;
        let visible_lines = self.visible_lines(&bounds);

        for (i, file_idx) in (self.scroll_offset..self.files.len())
            .take(visible_lines)
            .enumerate()
        {
            let file = &self.files[file_idx];
            let y = content_y + i as f32 * entry_height;
            let is_selected = self.selected == Some(file_idx);
            let is_hovered = self.hovered_index == Some(file_idx);

            // Hover highlight (subtle, below selection highlight)
            if is_hovered && !is_selected {
                let highlight_rect = Rect::new(bounds.x + 2.0, y - 1.0, bounds.width - 4.0, entry_height);
                output.spline_vertices.extend(create_rect_vertices(
                    &highlight_rect,
                    theme::SURFACE_HOVER.with_alpha(0.3).to_array(),
                ));
            }

            // Selection highlight
            if is_selected {
                let highlight_rect = Rect::new(bounds.x + 2.0, y - 1.0, bounds.width - 4.0, entry_height);
                output.spline_vertices.extend(create_rect_vertices(
                    &highlight_rect,
                    theme::ACCENT_MUTED.to_array(),
                ));
            }

            // Status indicator with background
            let status_color = match file.status {
                FileStatusKind::New => theme::STATUS_CLEAN,
                FileStatusKind::Modified => theme::STATUS_BEHIND,
                FileStatusKind::Deleted => theme::STATUS_DIRTY,
                FileStatusKind::Renamed => theme::BRANCH_PRIMARY,
                FileStatusKind::TypeChange => theme::BRANCH_HOTFIX,
            };

            let status_char = file.status.symbol().to_string();
            output.text_vertices.extend(text_renderer.layout_text(
                &status_char,
                bounds.x + 10.0,
                y + 2.0,
                status_color.to_array(),
            ));

            // File path - use brighter text when selected
            let max_path_width = bounds.width - 60.0;
            let char_width = text_renderer.char_width();
            let max_chars = (max_path_width / char_width) as usize;
            let path = if file.path.len() > max_chars && max_chars > 3 {
                format!("...{}", &file.path[file.path.len().saturating_sub(max_chars - 3)..])
            } else {
                file.path.clone()
            };

            let path_color = if is_selected || is_hovered {
                theme::TEXT_BRIGHT
            } else {
                theme::TEXT
            };
            output.text_vertices.extend(text_renderer.layout_text(
                &path,
                bounds.x + 28.0,
                y + 2.0,
                path_color.to_array(),
            ));

            // +/- counts on the right
            if file.additions > 0 || file.deletions > 0 {
                let stats = format!("+{} -{}", file.additions, file.deletions);
                let stats_x = bounds.right() - text_renderer.measure_text(&stats) - 10.0;
                output.text_vertices.extend(text_renderer.layout_text(
                    &stats,
                    stats_x,
                    y + 2.0,
                    theme::TEXT_MUTED.to_array(),
                ));
            }
        }

        // Empty state - centered and styled
        if self.files.is_empty() {
            let empty_text = if self.is_staged { "No staged changes" } else { "No changes" };
            let text_width = text_renderer.measure_text(empty_text);
            let center_x = bounds.x + (bounds.width - text_width) / 2.0;
            let center_y = content_y + entry_height * 2.0;
            output.text_vertices.extend(text_renderer.layout_text(
                empty_text,
                center_x,
                center_y,
                theme::TEXT_MUTED.to_array(),
            ));
        }

        output
    }

    fn focusable(&self) -> bool {
        true
    }

    fn set_focused(&mut self, focused: bool) {
        self.state.focused = focused;
    }
}
