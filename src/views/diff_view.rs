//! Diff viewer - displays color-coded diffs for commits and files

use crate::git::DiffFile;
use crate::input::{EventResponse, InputEvent};
use crate::ui::widget::{create_rect_vertices, theme, WidgetOutput};
use crate::ui::{Rect, TextRenderer};

/// Colors for diff rendering
mod diff_colors {
    use crate::ui::Color;

    pub const ADDITION_TEXT: Color = Color::rgba(0.298, 0.686, 0.314, 1.0);      // #4CAF50
    pub const ADDITION_BG: Color = Color::rgba(0.0, 0.235, 0.0, 0.3);            // dark green
    pub const DELETION_TEXT: Color = Color::rgba(0.937, 0.325, 0.314, 1.0);       // #EF5350
    pub const DELETION_BG: Color = Color::rgba(0.235, 0.0, 0.0, 0.3);            // dark red
    pub const HUNK_HEADER: Color = Color::rgba(0.671, 0.396, 0.859, 1.0);        // purple
    pub const FILE_HEADER_BG: Color = Color::rgba(0.180, 0.180, 0.180, 1.0);     // raised surface
    pub const LINE_NUMBER: Color = Color::rgba(0.400, 0.400, 0.400, 1.0);        // muted
}

/// View for displaying diffs
pub struct DiffView {
    /// Files to display
    diff_files: Vec<DiffFile>,
    /// Vertical scroll offset in pixels
    scroll_offset: f32,
    /// Total content height (computed during layout)
    content_height: f32,
    /// Title to show above the diff (e.g. commit summary)
    title: String,
}

impl DiffView {
    pub fn new() -> Self {
        Self {
            diff_files: Vec::new(),
            scroll_offset: 0.0,
            content_height: 0.0,
            title: String::new(),
        }
    }

    /// Load diff content to display
    pub fn set_diff(&mut self, diff_files: Vec<DiffFile>, title: String) {
        self.diff_files = diff_files;
        self.scroll_offset = 0.0;
        self.title = title;
    }

    /// Clear the diff view
    pub fn clear(&mut self) {
        self.diff_files.clear();
        self.scroll_offset = 0.0;
        self.title.clear();
    }

    /// Whether the diff view has content to show
    pub fn has_content(&self) -> bool {
        !self.diff_files.is_empty()
    }

    /// Handle input events
    pub fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        match event {
            InputEvent::Scroll { delta_y, x, y, .. } => {
                if bounds.contains(*x, *y) {
                    self.scroll_offset = (self.scroll_offset - delta_y)
                        .max(0.0)
                        .min((self.content_height - bounds.height).max(0.0));
                    return EventResponse::Consumed;
                }
                EventResponse::Ignored
            }
            _ => EventResponse::Ignored,
        }
    }

    /// Layout the diff view and produce rendering output
    pub fn layout(&mut self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

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
        let max_x = bounds.right() - padding;

        let mut y = bounds.y + padding - self.scroll_offset;
        let visible_top = bounds.y;
        let visible_bottom = bounds.bottom();

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

        for file in &self.diff_files {
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
                let stats = format!(
                    "  +{} -{}", file.additions, file.deletions
                );
                output.text_vertices.extend(text_renderer.layout_text(
                    &file.path,
                    bounds.x + padding,
                    y + 2.0,
                    theme::TEXT_BRIGHT.to_array(),
                ));

                let stats_x = bounds.x + padding + text_renderer.measure_text(&file.path);
                if stats_x + text_renderer.measure_text(&stats) < max_x {
                    // additions in green
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
            }
            y += header_height + 2.0;

            for hunk in &file.hunks {
                // Hunk header
                if y + line_height > visible_top && y < visible_bottom {
                    output.text_vertices.extend(text_renderer.layout_text(
                        &hunk.header,
                        bounds.x + padding,
                        y,
                        diff_colors::HUNK_HEADER.to_array(),
                    ));
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

                        // Line numbers in gutter
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

                        // Origin character (+, -, space)
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

                        // Line content (trimming trailing newline)
                        let content = line.content.trim_end_matches('\n');
                        // Truncate if too wide
                        let available_chars = ((max_x - content_x) / char_width) as usize;
                        let display_content = if content.len() > available_chars && available_chars > 3 {
                            &content[..available_chars]
                        } else {
                            content
                        };
                        output.text_vertices.extend(text_renderer.layout_text(
                            display_content,
                            content_x,
                            y,
                            text_color.to_array(),
                        ));
                    }
                    y += line_height;
                }
            }

            // Gap between files
            y += line_height / 2.0;
        }

        // Update total content height for scroll bounds
        self.content_height = y + self.scroll_offset - bounds.y;

        output
    }
}

impl Default for DiffView {
    fn default() -> Self {
        Self::new()
    }
}
