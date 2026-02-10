//! File list widget for staging area

use std::time::Instant;

use crate::git::{FileStatus, FileStatusKind};
use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{create_rect_vertices, create_rect_outline_vertices, create_rounded_rect_vertices, theme, Widget, WidgetId, WidgetOutput, WidgetState};
use crate::ui::widgets::scrollbar::{Scrollbar, ScrollAction};
use crate::ui::{Color, Rect, TextRenderer};

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
#[allow(dead_code)]
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
    /// Scrollbar widget
    scrollbar: Scrollbar,
    /// Display scale factor for HiDPI
    scale: f32,
    /// Last click time for double-click detection
    last_click_time: Option<Instant>,
    /// Last clicked file index for double-click detection
    last_click_index: Option<usize>,
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
            scrollbar: Scrollbar::new(),
            scale: 1.0,
            last_click_time: None,
            last_click_index: None,
        }
    }

    /// Returns true if any file item is currently hovered
    pub fn is_item_hovered(&self) -> bool {
        self.hovered_index.is_some()
    }

    /// Set the files to display
    pub fn set_files(&mut self, files: Vec<FileEntry>) {
        self.files = files;
        // Adjust selection if needed
        if let Some(idx) = self.selected
            && idx >= self.files.len() {
                self.selected = if self.files.is_empty() { None } else { Some(self.files.len() - 1) };
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

    /// Set the display scale factor for HiDPI scaling
    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale;
    }

    /// Consistent header height used across all methods
    fn header_height(&self) -> f32 {
        24.0 * self.scale
    }

    /// Consistent line/entry height used across all methods
    fn line_height(&self) -> f32 {
        22.0 * self.scale
    }

    fn visible_lines(&self, bounds: &Rect) -> usize {
        ((bounds.height - self.header_height()) / self.line_height()).max(1.0) as usize
    }

    /// Update hover state based on mouse position
    pub fn update_hover(&mut self, x: f32, y: f32, bounds: Rect) {
        if !bounds.contains(x, y) {
            self.hovered_index = None;
            return;
        }

        let header_height = self.header_height();
        let entry_height = self.line_height();
        let content_y = bounds.y + header_height + 6.0 * self.scale;

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

    /// Find which file is at the given Y position within these bounds.
    /// Uses the same layout values as update_hover() and layout().
    pub fn file_at_y(&self, y: f32, bounds: Rect) -> Option<String> {
        let header_height = self.header_height();
        let entry_height = self.line_height();
        let content_y = bounds.y + header_height + 6.0 * self.scale;

        let rel_y = y - content_y;
        if rel_y < 0.0 {
            return None;
        }

        let idx = self.scroll_offset + (rel_y / entry_height) as usize;
        self.files.get(idx).map(|f| f.path.clone())
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
        // Update scrollbar state
        let visible = self.visible_lines(&bounds);
        self.scrollbar.set_content(self.files.len(), visible, self.scroll_offset);

        // Scrollbar on right edge
        let scrollbar_width = 8.0;
        let (_content_bounds, scrollbar_bounds) = bounds.take_right(scrollbar_width);
        if self.scrollbar.handle_event(event, scrollbar_bounds).is_consumed() {
            if let Some(ScrollAction::ScrollTo(ratio)) = self.scrollbar.take_action() {
                let max_scroll = self.files.len().saturating_sub(visible);
                self.scroll_offset = (ratio * max_scroll as f32).round() as usize;
            }
            return EventResponse::Consumed;
        }

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
                    let header_height = self.header_height();
                    let entry_height = self.line_height();
                    let content_y = bounds.y + header_height;

                    if *y > content_y {
                        let clicked_line = ((*y - content_y) / entry_height) as usize;
                        let file_idx = self.scroll_offset + clicked_line;
                        if file_idx < self.files.len() {
                            // Double-click detection
                            let now = Instant::now();
                            if self.last_click_index == Some(file_idx)
                                && self.last_click_time.is_some_and(|t| now.duration_since(t).as_millis() < 400)
                            {
                                // Double-click: toggle stage
                                let path = self.files[file_idx].path.clone();
                                self.pending_action = Some(FileListAction::ToggleStage(path));
                                self.last_click_time = None;
                                self.last_click_index = None;
                                return EventResponse::Consumed;
                            }

                            // Single click: select and record for double-click
                            self.selected = Some(file_idx);
                            self.last_click_time = Some(now);
                            self.last_click_index = Some(file_idx);
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
                let scroll_lines = (-delta_y / 10.0) as i32;
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

        // Header background - slightly elevated with subtle tint
        let header_rect = Rect::new(bounds.x + 1.0, bounds.y + 1.0, bounds.width - 2.0, header_height);
        output.spline_vertices.extend(create_rounded_rect_vertices(
            &header_rect,
            theme::SURFACE_RAISED.lighten(0.03).to_array(),
            3.0,
        ));

        // Header: arrow icon and title text
        let arrow = if self.is_staged { "\u{25B2}" } else { "\u{25BC}" }; // ▲ Staged / ▼ Unstaged
        let title_text = format!("{} {}", arrow, self.title);
        output.text_vertices.extend(text_renderer.layout_text(
            &title_text,
            bounds.x + 10.0,
            bounds.y + 6.0,
            theme::TEXT_BRIGHT.to_array(),
        ));

        // File count pill badge (right of title)
        let count_text = format!("{}", self.files.len());
        let title_width = text_renderer.measure_text(&title_text);
        let count_width = text_renderer.measure_text(&count_text);
        let pill_pad_h = 6.0;
        let pill_pad_v = 2.0;
        let pill_x = bounds.x + 10.0 + title_width + 8.0;
        let pill_y = bounds.y + 6.0 - pill_pad_v;
        let pill_w = count_width + pill_pad_h * 2.0;
        let pill_h = line_height + pill_pad_v * 2.0;
        let pill_color = if self.is_staged {
            theme::STATUS_CLEAN.with_alpha(0.20)
        } else {
            theme::STATUS_BEHIND.with_alpha(0.20)
        };
        output.spline_vertices.extend(create_rounded_rect_vertices(
            &Rect::new(pill_x, pill_y, pill_w, pill_h),
            pill_color.to_array(),
            3.0,
        ));
        let pill_text_color = if self.is_staged {
            theme::STATUS_CLEAN
        } else {
            theme::STATUS_BEHIND
        };
        output.text_vertices.extend(text_renderer.layout_text(
            &count_text,
            pill_x + pill_pad_h,
            bounds.y + 6.0,
            pill_text_color.to_array(),
        ));

        // Underline accent on header (thin line at bottom of header bg)
        let accent_color = if self.is_staged {
            theme::STATUS_CLEAN.with_alpha(0.4)
        } else {
            theme::STATUS_BEHIND.with_alpha(0.4)
        };
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(bounds.x + 1.0, header_rect.bottom() - 2.0, bounds.width - 2.0, 2.0),
            accent_color.to_array(),
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

        // 1px separator line below header
        let sep_y = bounds.y + header_height;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(bounds.x + 1.0, sep_y, bounds.width - 2.0, 1.0),
            theme::BORDER.to_array(),
        ));

        // File entries
        let content_y = sep_y + 6.0 * self.scale;
        let entry_height = self.line_height();
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

            // Status indicator - colored letter
            let (status_color, status_letter) = match file.status {
                FileStatusKind::New =>        (Color::rgba(0.259, 0.647, 0.961, 1.0), "A"), // #42A5F5 blue
                FileStatusKind::Modified =>   (Color::rgba(0.400, 0.733, 0.416, 1.0), "M"), // #66BB6A green
                FileStatusKind::Deleted =>    (Color::rgba(0.937, 0.325, 0.314, 1.0), "D"), // #EF5350 red
                FileStatusKind::Renamed =>    (Color::rgba(0.149, 0.776, 0.855, 1.0), "R"), // #26C6DA cyan
                FileStatusKind::TypeChange => (Color::rgba(1.000, 0.718, 0.302, 1.0), "T"), // #FFB74D amber
            };

            output.text_vertices.extend(text_renderer.layout_text(
                status_letter,
                bounds.x + 10.0,
                y + 2.0,
                status_color.to_array(),
            ));

            // File path - split into directory (dim) and filename (bright)
            let path_x_offset = 26.0; // After status letter
            let max_path_width = bounds.width - path_x_offset - 30.0;
            let path = truncate_path_to_width(&file.path, text_renderer, max_path_width);

            let (dir_part, file_part) = match path.rfind('/') {
                Some(pos) => (&path[..=pos], &path[pos + 1..]),
                None => ("", path.as_str()),
            };

            let mut text_x = bounds.x + path_x_offset;

            // Directory portion in muted color
            if !dir_part.is_empty() {
                let dir_color = if is_selected || is_hovered {
                    theme::TEXT_MUTED.lighten(0.15)
                } else {
                    theme::TEXT_MUTED
                };
                output.text_vertices.extend(text_renderer.layout_text(
                    dir_part,
                    text_x,
                    y + 2.0,
                    dir_color.to_array(),
                ));
                text_x += text_renderer.measure_text(dir_part);
            }

            // Filename portion in bright color
            let file_color = if is_selected || is_hovered {
                theme::TEXT_BRIGHT
            } else {
                theme::TEXT
            };
            output.text_vertices.extend(text_renderer.layout_text(
                file_part,
                text_x,
                y + 2.0,
                file_color.to_array(),
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

        // Empty state - vertically centered in the content area below header
        if self.files.is_empty() {
            let content_area_top = sep_y + 1.0;
            let content_area_height = bounds.bottom() - content_area_top;

            let check_icon = "\u{2713}"; // ✓
            let empty_text = if self.is_staged {
                "No staged changes"
            } else {
                "Working tree clean"
            };
            let hint_text = if self.is_staged {
                "Stage files to commit them"
            } else {
                "Make changes to see them here"
            };

            let full_text = format!("{} {}", check_icon, empty_text);
            let text_width = text_renderer.measure_text(&full_text);
            let center_x = bounds.x + (bounds.width - text_width) / 2.0;
            // Offset slightly upward to make room for hint below
            let center_y = content_area_top + (content_area_height - line_height) / 2.0 - line_height * 0.6;

            // Muted green-tinted checkmark
            let check_color = if self.is_staged {
                theme::STATUS_CLEAN.with_alpha(0.5)
            } else {
                theme::STATUS_CLEAN.with_alpha(0.6)
            };
            output.text_vertices.extend(text_renderer.layout_text(
                check_icon,
                center_x,
                center_y,
                check_color.to_array(),
            ));
            let icon_w = text_renderer.measure_text(check_icon) + 4.0;
            output.text_vertices.extend(text_renderer.layout_text(
                empty_text,
                center_x + icon_w,
                center_y,
                theme::TEXT_MUTED.with_alpha(0.8).to_array(),
            ));

            // Hint text below (smaller, more muted)
            let hint_width = text_renderer.measure_text_scaled(hint_text, 0.85);
            let hint_x = bounds.x + (bounds.width - hint_width) / 2.0;
            let hint_y = center_y + line_height * 1.4;
            output.text_vertices.extend(text_renderer.layout_text_small(
                hint_text,
                hint_x,
                hint_y,
                theme::TEXT_MUTED.with_alpha(0.5).to_array(),
            ));
        }

        // Render scrollbar on right edge
        let scrollbar_width = 8.0;
        let (_content_area, scrollbar_bounds) = bounds.take_right(scrollbar_width);
        // Offset scrollbar below the header
        let scrollbar_area = Rect::new(
            scrollbar_bounds.x,
            scrollbar_bounds.y + header_height + 1.0,
            scrollbar_bounds.width,
            scrollbar_bounds.height - header_height - 1.0,
        );
        let scrollbar_output = self.scrollbar.layout(scrollbar_area);
        output.spline_vertices.extend(scrollbar_output.spline_vertices);

        output
    }

    fn focusable(&self) -> bool {
        true
    }

    fn set_focused(&mut self, focused: bool) {
        self.state.focused = focused;
    }
}

/// Truncate a file path to fit within max_width, preferring the filename end
fn truncate_path_to_width(path: &str, text_renderer: &TextRenderer, max_width: f32) -> String {
    if max_width <= 0.0 {
        return String::new();
    }
    let full_width = text_renderer.measure_text(path);
    if full_width <= max_width {
        return path.to_string();
    }
    let ellipsis = "...";
    let ellipsis_width = text_renderer.measure_text(ellipsis);
    let target_width = max_width - ellipsis_width;
    if target_width <= 0.0 {
        return ellipsis.to_string();
    }
    // Show as much of the end of the path as fits
    let mut width = 0.0;
    let chars: Vec<char> = path.chars().collect();
    let mut start = chars.len();
    while start > 0 {
        start -= 1;
        let cw = text_renderer.measure_text(&String::from(chars[start]));
        if width + cw > target_width {
            start += 1;
            break;
        }
        width += cw;
    }
    let suffix: String = chars[start..].iter().collect();
    format!("{}{}", ellipsis, suffix)
}
