//! Diff viewer - displays color-coded diffs for commits and files

use crate::git::DiffFile;
use crate::input::{EventResponse, InputEvent};
use crate::ui::widget::{create_rect_vertices, theme, WidgetOutput};
use crate::ui::{Color, Rect, TextRenderer};

/// Colors for diff rendering
mod diff_colors {
    use crate::ui::Color;

    pub const ADDITION_TEXT: Color = Color::rgba(0.298, 0.686, 0.314, 1.0);      // #4CAF50
    pub const ADDITION_BG: Color = Color::rgba(0.0, 0.235, 0.0, 0.3);            // dark green
    pub const ADDITION_HIGHLIGHT_BG: Color = Color::rgba(0.1, 0.4, 0.1, 0.55);   // brighter green
    pub const DELETION_TEXT: Color = Color::rgba(0.937, 0.325, 0.314, 1.0);       // #EF5350
    pub const DELETION_BG: Color = Color::rgba(0.235, 0.0, 0.0, 0.3);            // dark red
    pub const DELETION_HIGHLIGHT_BG: Color = Color::rgba(0.45, 0.08, 0.08, 0.55);// brighter red
    pub const HUNK_HEADER: Color = Color::rgba(0.671, 0.396, 0.859, 1.0);        // purple
    pub const FILE_HEADER_BG: Color = Color::rgba(0.180, 0.180, 0.180, 1.0);     // raised surface
    pub const LINE_NUMBER: Color = Color::rgba(0.400, 0.400, 0.400, 1.0);        // muted
    pub const STAGE_BUTTON_BG: Color = Color::rgba(0.180, 0.180, 0.220, 1.0);    // subtle blue-gray
    pub const STAGE_BUTTON_HOVER: Color = Color::rgba(0.220, 0.220, 0.280, 1.0); // brighter on hover
    pub const STAGE_BUTTON_TEXT: Color = Color::rgba(0.671, 0.396, 0.859, 1.0);  // purple like hunk headers
}

/// Action emitted by the diff view
#[derive(Clone, Debug)]
pub enum DiffAction {
    /// Stage a specific hunk (file_path, hunk_index)
    StageHunk(String, usize),
    /// Unstage a specific hunk (file_path, hunk_index)
    UnstageHunk(String, usize),
}

/// View for displaying diffs
pub struct DiffView {
    /// Files to display
    diff_files: Vec<DiffFile>,
    /// Vertical scroll offset in pixels
    scroll_offset: f32,
    /// Horizontal scroll offset in pixels
    h_scroll_offset: f32,
    /// Total content height (computed during layout)
    content_height: f32,
    /// Maximum content width (computed during layout)
    content_width: f32,
    /// Title to show above the diff (e.g. commit summary)
    title: String,
    /// Whether this is showing staged changes (affects stage/unstage button labels)
    showing_staged: bool,
    /// Pending action from a click
    pending_action: Option<DiffAction>,
    /// Hunk button bounds for click detection: (file_idx, hunk_idx, Rect)
    hunk_button_bounds: Vec<(usize, usize, Rect)>,
    /// Which hunk button is hovered
    hovered_hunk_button: Option<(usize, usize)>,
}

impl DiffView {
    pub fn new() -> Self {
        Self {
            diff_files: Vec::new(),
            scroll_offset: 0.0,
            h_scroll_offset: 0.0,
            content_height: 0.0,
            content_width: 0.0,
            title: String::new(),
            showing_staged: false,
            pending_action: None,
            hunk_button_bounds: Vec::new(),
            hovered_hunk_button: None,
        }
    }

    /// Load diff content to display
    pub fn set_diff(&mut self, diff_files: Vec<DiffFile>, title: String) {
        self.diff_files = diff_files;
        self.scroll_offset = 0.0;
        self.h_scroll_offset = 0.0;
        self.title = title;
        self.showing_staged = false;
        self.hunk_button_bounds.clear();
        self.hovered_hunk_button = None;
    }

    /// Load staged diff content
    pub fn set_staged_diff(&mut self, diff_files: Vec<DiffFile>, title: String) {
        self.set_diff(diff_files, title);
        self.showing_staged = true;
    }

    /// Clear the diff view
    pub fn clear(&mut self) {
        self.diff_files.clear();
        self.scroll_offset = 0.0;
        self.h_scroll_offset = 0.0;
        self.title.clear();
        self.hunk_button_bounds.clear();
        self.hovered_hunk_button = None;
    }

    /// Whether the diff view has content to show
    pub fn has_content(&self) -> bool {
        !self.diff_files.is_empty()
    }

    /// Take and clear any pending action
    pub fn take_action(&mut self) -> Option<DiffAction> {
        self.pending_action.take()
    }

    /// Handle input events
    pub fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        match event {
            InputEvent::Scroll { delta_x, delta_y, x, y, modifiers, .. } => {
                if bounds.contains(*x, *y) {
                    if modifiers.shift {
                        // Shift+Scroll = horizontal scroll
                        self.h_scroll_offset = (self.h_scroll_offset - delta_y)
                            .max(0.0)
                            .min((self.content_width - bounds.width).max(0.0));
                    } else {
                        // Normal scroll = vertical
                        self.scroll_offset = (self.scroll_offset - delta_y)
                            .max(0.0)
                            .min((self.content_height - bounds.height).max(0.0));
                        // Also handle native horizontal scroll (e.g. trackpads)
                        if delta_x.abs() > 0.5 {
                            self.h_scroll_offset = (self.h_scroll_offset - delta_x)
                                .max(0.0)
                                .min((self.content_width - bounds.width).max(0.0));
                        }
                    }
                    return EventResponse::Consumed;
                }
                EventResponse::Ignored
            }
            InputEvent::MouseDown { x, y, .. } => {
                if !bounds.contains(*x, *y) {
                    return EventResponse::Ignored;
                }
                // Check if a hunk stage button was clicked
                for &(file_idx, hunk_idx, ref btn_rect) in &self.hunk_button_bounds {
                    if btn_rect.contains(*x, *y)
                        && let Some(file) = self.diff_files.get(file_idx) {
                            let path = file.path.clone();
                            if self.showing_staged {
                                self.pending_action = Some(DiffAction::UnstageHunk(path, hunk_idx));
                            } else {
                                self.pending_action = Some(DiffAction::StageHunk(path, hunk_idx));
                            }
                            return EventResponse::Consumed;
                        }
                }
                EventResponse::Ignored
            }
            InputEvent::MouseMove { x, y, .. } => {
                if !bounds.contains(*x, *y) {
                    self.hovered_hunk_button = None;
                    return EventResponse::Ignored;
                }
                let mut found = None;
                for &(file_idx, hunk_idx, ref btn_rect) in &self.hunk_button_bounds {
                    if btn_rect.contains(*x, *y) {
                        found = Some((file_idx, hunk_idx));
                        break;
                    }
                }
                self.hovered_hunk_button = found;
                EventResponse::Ignored
            }
            InputEvent::KeyDown { key, modifiers, .. } => {
                if !bounds.contains(0.0, 0.0) && !self.has_content() {
                    return EventResponse::Ignored;
                }
                use crate::input::Key;
                match key {
                    Key::Left if !modifiers.any() => {
                        self.h_scroll_offset = (self.h_scroll_offset - 40.0).max(0.0);
                        EventResponse::Consumed
                    }
                    Key::Right if !modifiers.any() => {
                        self.h_scroll_offset = (self.h_scroll_offset + 40.0)
                            .min((self.content_width - 100.0).max(0.0));
                        EventResponse::Consumed
                    }
                    _ => EventResponse::Ignored,
                }
            }
            _ => EventResponse::Ignored,
        }
    }

    /// Layout the diff view and produce rendering output
    pub fn layout(&mut self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();
        self.hunk_button_bounds.clear();

        // Background
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            theme::SURFACE.to_array(),
        ));

        let padding = 8.0;
        let line_height = text_renderer.line_height();
        let char_width = text_renderer.char_width();
        let gutter_width = char_width * 8.0; // Space for two line numbers (4+4)
        let content_x = bounds.x + padding + gutter_width;

        let mut y = bounds.y + padding - self.scroll_offset;
        let visible_top = bounds.y;
        let visible_bottom = bounds.bottom();

        // Track max text width for horizontal scroll bounds
        let mut max_text_width: f32 = 0.0;

        // Title
        if !self.title.is_empty() {
            if y + line_height > visible_top && y < visible_bottom {
                output.text_vertices.extend(text_renderer.layout_text(
                    &self.title,
                    bounds.x + padding,
                    y,
                    theme::TEXT_BRIGHT.to_array(),
                ));
            }
            y += line_height + padding;
        }

        for (file_idx, file) in self.diff_files.iter().enumerate() {
            // File header
            let header_height = line_height + 4.0;
            if y + header_height > visible_top && y < visible_bottom {
                let header_rect = Rect::new(
                    bounds.x + 2.0,
                    y,
                    bounds.width - 4.0,
                    header_height,
                );
                output.spline_vertices.extend(create_rect_vertices(
                    &header_rect,
                    diff_colors::FILE_HEADER_BG.to_array(),
                ));

                // File path and stats
                output.text_vertices.extend(text_renderer.layout_text(
                    &file.path,
                    bounds.x + padding,
                    y + 2.0,
                    theme::TEXT_BRIGHT.to_array(),
                ));

                let stats_x = bounds.x + padding + text_renderer.measure_text(&file.path);
                let add_str = format!("  +{}", file.additions);
                output.text_vertices.extend(text_renderer.layout_text(
                    &add_str,
                    stats_x,
                    y + 2.0,
                    diff_colors::ADDITION_TEXT.to_array(),
                ));
                let del_x = stats_x + text_renderer.measure_text(&add_str);
                let del_str = format!(" -{}", file.deletions);
                output.text_vertices.extend(text_renderer.layout_text(
                    &del_str,
                    del_x,
                    y + 2.0,
                    diff_colors::DELETION_TEXT.to_array(),
                ));
            }
            y += header_height + 2.0;

            for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
                // Hunk header with stage/unstage button
                if y + line_height > visible_top && y < visible_bottom {
                    output.text_vertices.extend(text_renderer.layout_text(
                        &hunk.header,
                        bounds.x + padding - self.h_scroll_offset,
                        y,
                        diff_colors::HUNK_HEADER.to_array(),
                    ));

                    // Stage/Unstage hunk button (only for working directory diffs with a header)
                    if !hunk.header.is_empty() {
                        let btn_label = if self.showing_staged {
                            "Unstage Hunk"
                        } else {
                            "Stage Hunk"
                        };
                        let btn_w = text_renderer.measure_text(btn_label) + 12.0;
                        let btn_h = line_height;
                        let btn_x = bounds.right() - btn_w - padding;
                        let btn_y = y;
                        let btn_rect = Rect::new(btn_x, btn_y, btn_w, btn_h);

                        let is_hovered = self.hovered_hunk_button == Some((file_idx, hunk_idx));
                        let bg = if is_hovered {
                            diff_colors::STAGE_BUTTON_HOVER
                        } else {
                            diff_colors::STAGE_BUTTON_BG
                        };
                        output.spline_vertices.extend(create_rect_vertices(
                            &btn_rect,
                            bg.to_array(),
                        ));
                        output.text_vertices.extend(text_renderer.layout_text(
                            btn_label,
                            btn_x + 6.0,
                            btn_y,
                            diff_colors::STAGE_BUTTON_TEXT.to_array(),
                        ));

                        self.hunk_button_bounds.push((file_idx, hunk_idx, btn_rect));
                    }
                }
                y += line_height;

                // Lines
                for line in &hunk.lines {
                    if y + line_height > visible_top && y < visible_bottom {
                        let (text_color, bg_color) = match line.origin {
                            '+' => (diff_colors::ADDITION_TEXT, Some(diff_colors::ADDITION_BG)),
                            '-' => (diff_colors::DELETION_TEXT, Some(diff_colors::DELETION_BG)),
                            _ => (theme::TEXT, None),
                        };

                        // Background highlight for additions/deletions
                        if let Some(bg) = bg_color {
                            let line_rect = Rect::new(
                                bounds.x + 2.0,
                                y,
                                bounds.width - 4.0,
                                line_height,
                            );
                            output.spline_vertices.extend(create_rect_vertices(
                                &line_rect,
                                bg.to_array(),
                            ));
                        }

                        // Word-level highlight backgrounds
                        if !line.highlight_ranges.is_empty() {
                            let highlight_bg = match line.origin {
                                '+' => diff_colors::ADDITION_HIGHLIGHT_BG,
                                '-' => diff_colors::DELETION_HIGHLIGHT_BG,
                                _ => Color::TRANSPARENT,
                            };
                            let content_trimmed = line.content.trim_end_matches('\n');
                            for &(start, end) in &line.highlight_ranges {
                                if start >= content_trimmed.len() {
                                    continue;
                                }
                                let end = end.min(content_trimmed.len());
                                // Convert byte offsets to character-based x positions
                                let prefix_chars = content_trimmed[..start].chars().count();
                                let range_chars = content_trimmed[start..end].chars().count();
                                let hl_x = content_x + (prefix_chars as f32) * char_width - self.h_scroll_offset;
                                let hl_w = (range_chars as f32) * char_width;
                                if hl_x + hl_w > bounds.x && hl_x < bounds.right() {
                                    let hl_rect = Rect::new(hl_x, y, hl_w, line_height);
                                    output.spline_vertices.extend(create_rect_vertices(
                                        &hl_rect,
                                        highlight_bg.to_array(),
                                    ));
                                }
                            }
                        }

                        // Line numbers in gutter (not affected by h_scroll)
                        let gutter_x = bounds.x + padding;
                        if let Some(old) = line.old_lineno {
                            let num_str = format!("{:>4}", old);
                            output.text_vertices.extend(text_renderer.layout_text(
                                &num_str,
                                gutter_x,
                                y,
                                diff_colors::LINE_NUMBER.to_array(),
                            ));
                        }
                        if let Some(new) = line.new_lineno {
                            let num_str = format!("{:>4}", new);
                            output.text_vertices.extend(text_renderer.layout_text(
                                &num_str,
                                gutter_x + char_width * 4.0,
                                y,
                                diff_colors::LINE_NUMBER.to_array(),
                            ));
                        }

                        // Origin character (+, -, space) - not affected by h_scroll
                        let origin_str = format!("{}", line.origin);
                        let origin_color = match line.origin {
                            '+' => diff_colors::ADDITION_TEXT,
                            '-' => diff_colors::DELETION_TEXT,
                            _ => diff_colors::LINE_NUMBER,
                        };
                        output.text_vertices.extend(text_renderer.layout_text(
                            &origin_str,
                            content_x - char_width * 1.5,
                            y,
                            origin_color.to_array(),
                        ));

                        // Line content with horizontal scroll (no truncation)
                        let content = line.content.trim_end_matches('\n');
                        let text_x = content_x - self.h_scroll_offset;
                        let text_width = text_renderer.measure_text(content);
                        max_text_width = max_text_width.max(text_width + gutter_width + padding * 2.0);

                        // Only render if some of the text is visible
                        if text_x + text_width > bounds.x && text_x < bounds.right() {
                            output.text_vertices.extend(text_renderer.layout_text(
                                content,
                                text_x,
                                y,
                                text_color.to_array(),
                            ));
                        }
                    }
                    y += line_height;
                }
            }

            // Gap between files
            y += line_height / 2.0;
        }

        // Update total content dimensions for scroll bounds
        self.content_height = y + self.scroll_offset - bounds.y;
        self.content_width = max_text_width;

        output
    }
}

impl Default for DiffView {
    fn default() -> Self {
        Self::new()
    }
}
