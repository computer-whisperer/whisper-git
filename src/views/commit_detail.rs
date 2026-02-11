//! Commit detail panel - shows commit metadata and file list

use git2::Oid;

use crate::git::{DiffFile, FullCommitInfo};
use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{create_rect_vertices, theme, WidgetOutput};
use crate::ui::{Rect, TextRenderer};
use crate::ui::text_util::truncate_to_width;

/// Actions emitted by the commit detail view
#[derive(Clone, Debug)]
pub enum CommitDetailAction {
    ViewFileDiff(Oid, String),
}

/// View showing commit metadata and a list of changed files
pub struct CommitDetailView {
    /// Full commit information
    commit_info: Option<FullCommitInfo>,
    /// Files changed in this commit (summary only, not full diffs)
    changed_files: Vec<DiffFile>,
    /// Currently selected file index
    selected_file: Option<usize>,
    /// Scroll offset for the file list
    file_scroll_offset: f32,
    /// Total file list content height
    file_content_height: f32,
    /// Pending action
    pending_action: Option<CommitDetailAction>,
    /// Cached line height
    line_height: f32,
}

impl CommitDetailView {
    pub fn new() -> Self {
        Self {
            commit_info: None,
            changed_files: Vec::new(),
            selected_file: None,
            file_scroll_offset: 0.0,
            file_content_height: 0.0,
            pending_action: None,
            line_height: 18.0,
        }
    }

    /// Set the commit to display
    pub fn set_commit(&mut self, info: FullCommitInfo, diff_files: Vec<DiffFile>) {
        self.commit_info = Some(info);
        self.changed_files = diff_files;
        self.selected_file = if self.changed_files.is_empty() { None } else { Some(0) };
        self.file_scroll_offset = 0.0;
    }

    /// Clear the detail view
    pub fn clear(&mut self) {
        self.commit_info = None;
        self.changed_files.clear();
        self.selected_file = None;
        self.file_scroll_offset = 0.0;
    }

    /// Whether the detail view has content
    pub fn has_content(&self) -> bool {
        self.commit_info.is_some()
    }

    /// Take pending action
    pub fn take_action(&mut self) -> Option<CommitDetailAction> {
        self.pending_action.take()
    }

    /// Compute the metadata and file list regions
    fn compute_regions(&self, bounds: Rect) -> (Rect, Rect) {
        // Metadata gets top portion (about 120px or 30% whichever is smaller)
        let meta_height = (bounds.height * 0.30).min(120.0);
        let meta_rect = Rect::new(bounds.x, bounds.y, bounds.width, meta_height);
        let file_rect = Rect::new(bounds.x, bounds.y + meta_height, bounds.width, bounds.height - meta_height);
        (meta_rect, file_rect)
    }

    /// Emit action for the currently selected file
    fn emit_file_action(&mut self) {
        if let (Some(info), Some(idx)) = (&self.commit_info, self.selected_file)
            && let Some(file) = self.changed_files.get(idx) {
                self.pending_action = Some(CommitDetailAction::ViewFileDiff(
                    info.id,
                    file.path.clone(),
                ));
            }
    }

    /// Handle input events
    pub fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        if self.commit_info.is_none() {
            return EventResponse::Ignored;
        }

        let (_meta_rect, file_rect) = self.compute_regions(bounds);

        match event {
            InputEvent::Scroll { delta_y, x, y, .. } => {
                if bounds.contains(*x, *y) {
                    self.file_scroll_offset = (self.file_scroll_offset - delta_y * 2.0)
                        .max(0.0)
                        .min((self.file_content_height - file_rect.height).max(0.0));
                    return EventResponse::Consumed;
                }
            }
            InputEvent::KeyDown { key, .. } => {
                match key {
                    Key::J | Key::Down => {
                        if let Some(idx) = self.selected_file {
                            if idx + 1 < self.changed_files.len() {
                                self.selected_file = Some(idx + 1);
                                self.emit_file_action();
                            }
                        } else if !self.changed_files.is_empty() {
                            self.selected_file = Some(0);
                            self.emit_file_action();
                        }
                        return EventResponse::Consumed;
                    }
                    Key::K | Key::Up => {
                        if let Some(idx) = self.selected_file
                            && idx > 0 {
                                self.selected_file = Some(idx - 1);
                                self.emit_file_action();
                            }
                        return EventResponse::Consumed;
                    }
                    Key::Enter => {
                        self.emit_file_action();
                        return EventResponse::Consumed;
                    }
                    _ => {}
                }
            }
            InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } => {
                if file_rect.contains(*x, *y) {
                    // Find which file was clicked
                    let line_height = self.line_height;
                    let padding = 8.0;
                    let mut item_y = file_rect.y + padding - self.file_scroll_offset;

                    // Skip the "Files changed" header
                    item_y += line_height + 4.0;

                    for (idx, _file) in self.changed_files.iter().enumerate() {
                        if *y >= item_y && *y < item_y + line_height {
                            self.selected_file = Some(idx);
                            self.emit_file_action();
                            return EventResponse::Consumed;
                        }
                        item_y += line_height;
                    }
                    return EventResponse::Consumed;
                }
            }
            _ => {}
        }
        EventResponse::Ignored
    }

    /// Layout and render the commit detail panel
    pub fn layout(&mut self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();
        self.line_height = text_renderer.line_height();

        let Some(info) = &self.commit_info else {
            return output;
        };

        // Background
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            theme::SURFACE.to_array(),
        ));

        let (meta_rect, file_rect) = self.compute_regions(bounds);
        let padding = 8.0;
        let line_height = self.line_height;
        let char_width = text_renderer.char_width();

        // --- Metadata Section ---
        let meta_inner = meta_rect.inset(padding);
        let mut y = meta_inner.y;

        // SHA line
        let sha_label = format!("SHA: {}", info.short_id);
        output.text_vertices.extend(text_renderer.layout_text(
            &sha_label,
            meta_inner.x,
            y,
            theme::TEXT_MUTED.to_array(),
        ));
        // Full SHA on the right (truncated if needed)
        let full_sha = info.id.to_string();
        let sha_width = text_renderer.measure_text(&full_sha);
        let sha_x = (meta_inner.right() - sha_width).max(meta_inner.x + text_renderer.measure_text(&sha_label) + 16.0);
        output.text_vertices.extend(text_renderer.layout_text(
            &full_sha,
            sha_x,
            y,
            theme::TEXT.to_array(),
        ));
        y += line_height;

        // Author line
        let author_line = format!("Author: {} <{}>", info.author_name, info.author_email);
        output.text_vertices.extend(text_renderer.layout_text(
            &author_line,
            meta_inner.x,
            y,
            theme::TEXT.to_array(),
        ));
        // Time on the right
        let time_str = info.relative_author_time();
        let time_width = text_renderer.measure_text(&time_str);
        output.text_vertices.extend(text_renderer.layout_text(
            &time_str,
            meta_inner.right() - time_width,
            y,
            theme::TEXT_MUTED.to_array(),
        ));
        y += line_height;

        // Parents line
        if !info.parent_short_ids.is_empty() {
            let parents_str = format!("Parents: {}", info.parent_short_ids.join(", "));
            output.text_vertices.extend(text_renderer.layout_text(
                &parents_str,
                meta_inner.x,
                y,
                theme::TEXT_MUTED.to_array(),
            ));
            y += line_height;
        }

        // Summary / commit message
        y += 4.0; // small gap
        let message = &info.full_message;
        for msg_line in message.lines().take(4) {
            if y + line_height > meta_rect.bottom() {
                break;
            }
            // Truncate long lines
            let display = truncate_to_width(msg_line, text_renderer, meta_inner.width);
            output.text_vertices.extend(text_renderer.layout_text(
                &display,
                meta_inner.x,
                y,
                theme::TEXT_BRIGHT.to_array(),
            ));
            y += line_height;
        }

        // --- Divider between metadata and file list ---
        let divider_rect = Rect::new(bounds.x + 4.0, file_rect.y - 1.0, bounds.width - 8.0, 1.0);
        output.spline_vertices.extend(create_rect_vertices(
            &divider_rect,
            theme::BORDER.to_array(),
        ));

        // --- File List Section ---
        let file_inner = file_rect.inset(padding);
        let mut fy = file_inner.y - self.file_scroll_offset;
        let visible_top = file_rect.y;
        let visible_bottom = file_rect.bottom();

        // Section header
        let files_header = format!("Files changed ({})", self.changed_files.len());
        if fy + line_height > visible_top && fy < visible_bottom {
            output.text_vertices.extend(text_renderer.layout_text(
                &files_header,
                file_inner.x,
                fy,
                theme::TEXT_MUTED.to_array(),
            ));
        }
        fy += line_height + 4.0;

        for (idx, file) in self.changed_files.iter().enumerate() {
            if fy >= visible_bottom {
                break;
            }
            if fy + line_height > visible_top {
                let is_selected = self.selected_file == Some(idx);

                // Selection highlight
                if is_selected {
                    let highlight_rect = Rect::new(
                        file_inner.x - 4.0,
                        fy,
                        file_inner.width + 8.0,
                        line_height,
                    );
                    output.spline_vertices.extend(create_rect_vertices(
                        &highlight_rect,
                        theme::ACCENT_MUTED.to_array(),
                    ));
                }

                // File path
                let path_color = if is_selected {
                    theme::TEXT_BRIGHT.to_array()
                } else {
                    theme::TEXT.to_array()
                };

                // Stats first: +N -N
                let add_str = format!("+{}", file.additions);
                let del_str = format!("-{}", file.deletions);

                // Right-align stats
                let del_width = text_renderer.measure_text(&del_str);
                let add_width = text_renderer.measure_text(&add_str);
                let stats_gap = char_width;

                let del_x = file_inner.right() - del_width;
                let add_x = del_x - stats_gap - add_width;

                // Truncate path if needed
                let max_path_width = add_x - file_inner.x - char_width * 2.0;
                let path_display = truncate_path(&file.path, text_renderer, max_path_width);

                output.text_vertices.extend(text_renderer.layout_text(
                    &path_display,
                    file_inner.x + 8.0,
                    fy,
                    path_color,
                ));

                // Addition count in green
                output.text_vertices.extend(text_renderer.layout_text(
                    &add_str,
                    add_x,
                    fy,
                    [0.298, 0.686, 0.314, 1.0], // green
                ));

                // Deletion count in red
                output.text_vertices.extend(text_renderer.layout_text(
                    &del_str,
                    del_x,
                    fy,
                    [0.937, 0.325, 0.314, 1.0], // red
                ));
            }
            fy += line_height;
        }

        // Track file list content height for scrolling
        self.file_content_height = (self.changed_files.len() as f32 + 1.5) * line_height;

        output
    }
}

impl Default for CommitDetailView {
    fn default() -> Self {
        Self::new()
    }
}

/// Truncate a file path to fit within max_width
fn truncate_path(path: &str, text_renderer: &TextRenderer, max_width: f32) -> String {
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

    // Prefer showing the filename part
    if let Some(pos) = path.rfind('/') {
        let filename = &path[pos + 1..];
        let filename_width = text_renderer.measure_text(filename);
        if filename_width + ellipsis_width <= max_width {
            return format!("...{}", &path[pos..]);
        }
    }

    // Fall back to truncating from the end
    let chars: Vec<char> = path.chars().collect();
    let mut end = chars.len();
    while end > 0 {
        let truncated: String = chars[..end].iter().collect();
        if text_renderer.measure_text(&truncated) <= target_width {
            return format!("{}{}", truncated, ellipsis);
        }
        end -= 1;
    }
    ellipsis.to_string()
}

