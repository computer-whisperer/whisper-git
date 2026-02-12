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
    pub const DISCARD_BUTTON_BG: Color = Color::rgba(0.280, 0.120, 0.120, 1.0);    // dark red
    pub const DISCARD_BUTTON_HOVER: Color = Color::rgba(0.360, 0.140, 0.140, 1.0); // brighter red on hover
    pub const DISCARD_BUTTON_TEXT: Color = Color::rgba(0.937, 0.325, 0.314, 1.0);  // red text
}

/// Action emitted by the diff view
#[derive(Clone, Debug)]
pub enum DiffAction {
    /// Stage a specific hunk (file_path, hunk_index)
    StageHunk(String, usize),
    /// Unstage a specific hunk (file_path, hunk_index)
    UnstageHunk(String, usize),
    /// Discard a specific hunk from the working tree (file_path, hunk_index)
    DiscardHunk(String, usize),
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
    /// Discard hunk button bounds for click detection: (file_idx, hunk_idx, Rect)
    discard_button_bounds: Vec<(usize, usize, Rect)>,
    /// Which discard button is hovered
    hovered_discard_button: Option<(usize, usize)>,
    /// Y offsets of file headers (relative to content start), computed during layout
    file_y_offsets: Vec<f32>,
    /// Y offsets of hunk headers (relative to content start), computed during layout
    hunk_y_offsets: Vec<f32>,
    /// Last known viewport height for page scrolling
    viewport_height: f32,
    /// Line height from last layout (for line-by-line scrolling)
    last_line_height: f32,
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
            discard_button_bounds: Vec::new(),
            hovered_discard_button: None,
            file_y_offsets: Vec::new(),
            hunk_y_offsets: Vec::new(),
            viewport_height: 0.0,
            last_line_height: 20.0,
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
        self.discard_button_bounds.clear();
        self.hovered_discard_button = None;
        self.file_y_offsets.clear();
        self.hunk_y_offsets.clear();
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
        self.discard_button_bounds.clear();
        self.hovered_discard_button = None;
        self.file_y_offsets.clear();
        self.hunk_y_offsets.clear();
    }

    /// Whether the diff view has content to show
    pub fn has_content(&self) -> bool {
        !self.diff_files.is_empty()
    }

    /// Get the current title text
    pub fn title(&self) -> &str {
        &self.title
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
                        self.h_scroll_offset = (self.h_scroll_offset - delta_y * 2.0)
                            .max(0.0)
                            .min((self.content_width - bounds.width).max(0.0));
                    } else {
                        // Normal scroll = vertical
                        self.scroll_offset = (self.scroll_offset - delta_y * 2.0)
                            .max(0.0)
                            .min((self.content_height - bounds.height).max(0.0));
                        // Also handle native horizontal scroll (e.g. trackpads)
                        if delta_x.abs() > 0.5 {
                            self.h_scroll_offset = (self.h_scroll_offset - delta_x * 2.0)
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
                // Check if a discard hunk button was clicked
                for &(file_idx, hunk_idx, ref btn_rect) in &self.discard_button_bounds {
                    if btn_rect.contains(*x, *y)
                        && let Some(file) = self.diff_files.get(file_idx) {
                            let path = file.path.clone();
                            self.pending_action = Some(DiffAction::DiscardHunk(path, hunk_idx));
                            return EventResponse::Consumed;
                        }
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
                    self.hovered_discard_button = None;
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
                let mut discard_found = None;
                for &(file_idx, hunk_idx, ref btn_rect) in &self.discard_button_bounds {
                    if btn_rect.contains(*x, *y) {
                        discard_found = Some((file_idx, hunk_idx));
                        break;
                    }
                }
                self.hovered_discard_button = discard_found;
                EventResponse::Ignored
            }
            InputEvent::KeyDown { key, modifiers, .. } => {
                if !self.has_content() {
                    return EventResponse::Ignored;
                }
                use crate::input::Key;
                let max_scroll = (self.content_height - bounds.height).max(0.0);
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
                    // j / Down: scroll down one line
                    Key::J | Key::Down if !modifiers.any() => {
                        self.scroll_offset = (self.scroll_offset + self.last_line_height)
                            .min(max_scroll);
                        EventResponse::Consumed
                    }
                    // k / Up: scroll up one line
                    Key::K | Key::Up if !modifiers.any() => {
                        self.scroll_offset = (self.scroll_offset - self.last_line_height)
                            .max(0.0);
                        EventResponse::Consumed
                    }
                    // PageDown: scroll down one page
                    Key::PageDown if !modifiers.any() => {
                        self.scroll_offset = (self.scroll_offset + self.viewport_height * 0.9)
                            .min(max_scroll);
                        EventResponse::Consumed
                    }
                    // PageUp: scroll up one page
                    Key::PageUp if !modifiers.any() => {
                        self.scroll_offset = (self.scroll_offset - self.viewport_height * 0.9)
                            .max(0.0);
                        EventResponse::Consumed
                    }
                    // Home: jump to start
                    Key::Home if !modifiers.any() => {
                        self.scroll_offset = 0.0;
                        EventResponse::Consumed
                    }
                    // End: jump to end
                    Key::End if !modifiers.any() => {
                        self.scroll_offset = max_scroll;
                        EventResponse::Consumed
                    }
                    // n: jump to next hunk
                    Key::N if !modifiers.any() => {
                        self.jump_to_next_hunk();
                        EventResponse::Consumed
                    }
                    // p / Shift+N: jump to previous hunk
                    Key::P if !modifiers.any() => {
                        self.jump_to_prev_hunk();
                        EventResponse::Consumed
                    }
                    Key::N if modifiers.shift && !modifiers.ctrl && !modifiers.alt => {
                        self.jump_to_prev_hunk();
                        EventResponse::Consumed
                    }
                    // ]: jump to next file
                    Key::RightBracket if !modifiers.any() => {
                        self.jump_to_next_file();
                        EventResponse::Consumed
                    }
                    // [: jump to previous file
                    Key::LeftBracket if !modifiers.any() => {
                        self.jump_to_prev_file();
                        EventResponse::Consumed
                    }
                    _ => EventResponse::Ignored,
                }
            }
            _ => EventResponse::Ignored,
        }
    }

    /// Jump scroll_offset to the next hunk after the current position
    fn jump_to_next_hunk(&mut self) {
        let current = self.scroll_offset;
        for &offset in &self.hunk_y_offsets {
            if offset > current + 1.0 {
                let max_scroll = (self.content_height - self.viewport_height).max(0.0);
                self.scroll_offset = offset.min(max_scroll);
                return;
            }
        }
    }

    /// Jump scroll_offset to the previous hunk before the current position
    fn jump_to_prev_hunk(&mut self) {
        let current = self.scroll_offset;
        for &offset in self.hunk_y_offsets.iter().rev() {
            if offset < current - 1.0 {
                self.scroll_offset = offset.max(0.0);
                return;
            }
        }
        // If no previous hunk found, go to top
        self.scroll_offset = 0.0;
    }

    /// Jump scroll_offset to the next file header after the current position
    fn jump_to_next_file(&mut self) {
        let current = self.scroll_offset;
        for &offset in &self.file_y_offsets {
            if offset > current + 1.0 {
                let max_scroll = (self.content_height - self.viewport_height).max(0.0);
                self.scroll_offset = offset.min(max_scroll);
                return;
            }
        }
    }

    /// Jump scroll_offset to the previous file header before the current position
    fn jump_to_prev_file(&mut self) {
        let current = self.scroll_offset;
        for &offset in self.file_y_offsets.iter().rev() {
            if offset < current - 1.0 {
                self.scroll_offset = offset.max(0.0);
                return;
            }
        }
        // If no previous file found, go to top
        self.scroll_offset = 0.0;
    }

    /// Layout the diff view and produce rendering output
    pub fn layout(&mut self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();
        self.hunk_button_bounds.clear();
        self.discard_button_bounds.clear();
        self.file_y_offsets.clear();
        self.hunk_y_offsets.clear();

        // Background
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            theme::SURFACE.to_array(),
        ));

        let padding = 8.0;
        let line_height = text_renderer.line_height();
        self.last_line_height = line_height;
        self.viewport_height = bounds.height;
        let digit_width = text_renderer.measure_text("0");
        let gutter_width = digit_width * 8.0; // Space for two line numbers (4+4)
        let content_x = bounds.x + padding + gutter_width;

        let mut y = bounds.y + padding - self.scroll_offset;
        let visible_top = bounds.y;
        let visible_bottom = bounds.bottom();
        // content_y tracks position relative to content start (for navigation offsets)
        let mut content_y = padding;

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
            content_y += line_height + padding;
        }

        for (file_idx, file) in self.diff_files.iter().enumerate() {
            // Record file header offset for navigation
            self.file_y_offsets.push(content_y);

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
            content_y += header_height + 2.0;

            for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
                // Record hunk header offset for navigation
                self.hunk_y_offsets.push(content_y);

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

                        // Discard Hunk button (only for unstaged diffs, to the left of Stage Hunk)
                        if !self.showing_staged {
                            let discard_label = "Discard Hunk";
                            let discard_w = text_renderer.measure_text(discard_label) + 12.0;
                            let discard_x = btn_x - discard_w - 6.0;
                            let discard_rect = Rect::new(discard_x, btn_y, discard_w, btn_h);

                            let is_discard_hovered = self.hovered_discard_button == Some((file_idx, hunk_idx));
                            let discard_bg = if is_discard_hovered {
                                diff_colors::DISCARD_BUTTON_HOVER
                            } else {
                                diff_colors::DISCARD_BUTTON_BG
                            };
                            output.spline_vertices.extend(create_rect_vertices(
                                &discard_rect,
                                discard_bg.to_array(),
                            ));
                            output.text_vertices.extend(text_renderer.layout_text(
                                discard_label,
                                discard_x + 6.0,
                                btn_y,
                                diff_colors::DISCARD_BUTTON_TEXT.to_array(),
                            ));

                            self.discard_button_bounds.push((file_idx, hunk_idx, discard_rect));
                        }
                    }
                }
                y += line_height;
                content_y += line_height;

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
                                // Measure actual text widths for proportional font
                                let hl_x = content_x + text_renderer.measure_text(&content_trimmed[..start]) - self.h_scroll_offset;
                                let hl_w = text_renderer.measure_text(&content_trimmed[start..end]);
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
                                gutter_x + digit_width * 4.0,
                                y,
                                diff_colors::LINE_NUMBER.to_array(),
                            ));
                        }

                        // Origin character (+, -, space) - not affected by h_scroll
                        let origin_str: String = line.origin.into();
                        let origin_color = match line.origin {
                            '+' => diff_colors::ADDITION_TEXT,
                            '-' => diff_colors::DELETION_TEXT,
                            _ => diff_colors::LINE_NUMBER,
                        };
                        output.text_vertices.extend(text_renderer.layout_text(
                            &origin_str,
                            content_x - digit_width * 1.5,
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
                    content_y += line_height;
                }
            }

            // Gap between files
            y += line_height / 2.0;
            content_y += line_height / 2.0;
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
