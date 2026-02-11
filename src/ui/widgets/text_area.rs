//! Multi-line text area widget

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{create_rect_outline_vertices, create_rect_vertices, theme, Widget, WidgetOutput, WidgetState};
use crate::ui::{Rect, TextRenderer};

/// Find the byte offset of the previous word boundary from cursor position within a line.
fn word_boundary_left(text: &str, cursor: usize) -> usize {
    let before = &text[..cursor];
    let mut chars: Vec<(usize, char)> = before.char_indices().collect();
    if chars.is_empty() {
        return 0;
    }
    // Skip whitespace/non-alnum going left
    while let Some(&(_, c)) = chars.last() {
        if c.is_alphanumeric() {
            break;
        }
        chars.pop();
    }
    // Skip alnum going left
    while let Some(&(_, c)) = chars.last() {
        if !c.is_alphanumeric() {
            break;
        }
        chars.pop();
    }
    chars.last().map(|&(i, c)| i + c.len_utf8()).unwrap_or(0)
}

/// Find the byte offset of the next word boundary from cursor position within a line.
fn word_boundary_right(text: &str, cursor: usize) -> usize {
    let after = &text[cursor..];
    let mut iter = after.char_indices();
    // Skip alnum going right
    let mut offset = 0;
    for (i, c) in iter.by_ref() {
        if !c.is_alphanumeric() {
            offset = i;
            break;
        }
        offset = i + c.len_utf8();
    }
    // If we consumed all alnum chars, check if we stopped in non-alnum
    let remaining = &after[offset..];
    if remaining.is_empty() {
        return cursor + offset;
    }
    let first_remaining = remaining.chars().next().unwrap();
    if !first_remaining.is_alphanumeric() {
        for (i, c) in remaining.char_indices() {
            if c.is_alphanumeric() {
                return cursor + offset + i;
            }
        }
        return text.len();
    }
    cursor + offset
}

/// A multi-line text editing area
pub struct TextArea {
    state: WidgetState,
    /// Lines of text
    lines: Vec<String>,
    /// Cursor position (line, column)
    cursor_line: usize,
    cursor_col: usize,
    /// Selection anchor (line, col) - set when shift+arrow or Ctrl+A starts selection
    selection_start: Option<(usize, usize)>,
    /// Scroll offset in lines
    scroll_offset: usize,
    /// Whether the content was modified
    modified: bool,
    /// Guard against double-insertion: set when KeyDown inserts text,
    /// cleared when TextInput fires for the same keystroke
    inserted_from_key: bool,
    /// Whether the cursor is currently visible (for blinking)
    cursor_visible: bool,
    /// Last time the cursor blink state changed
    last_blink: std::time::Instant,
}

impl TextArea {
    pub fn new() -> Self {
        Self {
            state: WidgetState::new(),
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
            selection_start: None,
            scroll_offset: 0,
            modified: false,
            inserted_from_key: false,
            cursor_visible: true,
            last_blink: std::time::Instant::now(),
        }
    }

    /// Set the text content
    pub fn set_text(&mut self, text: &str) {
        self.lines = text.lines().map(|s| s.to_string()).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.selection_start = None;
        self.scroll_offset = 0;
    }

    /// Get the full text content
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    fn current_line(&self) -> &String {
        &self.lines[self.cursor_line]
    }

    /// Get the selected text, if any
    fn selected_text(&self) -> Option<String> {
        let (sl, sc) = self.selection_start?;
        let (el, ec) = (self.cursor_line, self.cursor_col);
        // Determine start/end in document order
        let (start_line, start_col, end_line, end_col) =
            if (sl, sc) <= (el, ec) { (sl, sc, el, ec) } else { (el, ec, sl, sc) };
        if start_line == end_line {
            let line = &self.lines[start_line];
            Some(line[start_col..end_col].to_string())
        } else {
            let mut result = String::new();
            result.push_str(&self.lines[start_line][start_col..]);
            for l in (start_line + 1)..end_line {
                result.push('\n');
                result.push_str(&self.lines[l]);
            }
            result.push('\n');
            result.push_str(&self.lines[end_line][..end_col]);
            Some(result)
        }
    }

    /// Delete the selected text and collapse cursor to selection start
    fn delete_selection(&mut self) {
        let (sl, sc) = match self.selection_start {
            Some(s) => s,
            None => return,
        };
        let (el, ec) = (self.cursor_line, self.cursor_col);
        let (start_line, start_col, end_line, end_col) =
            if (sl, sc) <= (el, ec) { (sl, sc, el, ec) } else { (el, ec, sl, sc) };

        if start_line == end_line {
            self.lines[start_line].drain(start_col..end_col);
        } else {
            // Keep the part before selection on start_line and after selection on end_line
            let tail = self.lines[end_line][end_col..].to_string();
            self.lines[start_line].truncate(start_col);
            self.lines[start_line].push_str(&tail);
            // Remove intermediate + end lines
            self.lines.drain((start_line + 1)..=end_line);
        }
        self.cursor_line = start_line;
        self.cursor_col = start_col;
        self.selection_start = None;
        self.modified = true;
    }

    fn insert_char(&mut self, c: char) {
        let line = &mut self.lines[self.cursor_line];
        line.insert(self.cursor_col, c);
        self.cursor_col += 1;
        self.modified = true;
    }

    fn insert_newline(&mut self) {
        let current_line = &mut self.lines[self.cursor_line];
        let rest = current_line.split_off(self.cursor_col);
        self.cursor_line += 1;
        self.cursor_col = 0;
        self.lines.insert(self.cursor_line, rest);
        self.modified = true;
    }

    fn delete_backward(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
            self.lines[self.cursor_line].remove(self.cursor_col);
            self.modified = true;
        } else if self.cursor_line > 0 {
            // Join with previous line
            let current_line = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].len();
            self.lines[self.cursor_line].push_str(&current_line);
            self.modified = true;
        }
    }

    fn delete_forward(&mut self) {
        let line_len = self.current_line().len();
        if self.cursor_col < line_len {
            self.lines[self.cursor_line].remove(self.cursor_col);
            self.modified = true;
        } else if self.cursor_line < self.lines.len() - 1 {
            // Join with next line
            let next_line = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].push_str(&next_line);
            self.modified = true;
        }
    }

    fn move_cursor(&mut self, dx: i32, dy: i32) {
        // Vertical movement
        if dy != 0 {
            if dy < 0 {
                self.cursor_line = self.cursor_line.saturating_sub((-dy) as usize);
            } else {
                self.cursor_line = (self.cursor_line + dy as usize).min(self.lines.len() - 1);
            }
            // Clamp column to line length
            self.cursor_col = self.cursor_col.min(self.current_line().len());
        }

        // Horizontal movement
        if dx != 0 {
            if dx < 0 {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                } else if self.cursor_line > 0 {
                    self.cursor_line -= 1;
                    self.cursor_col = self.current_line().len();
                }
            } else {
                let line_len = self.current_line().len();
                if self.cursor_col < line_len {
                    self.cursor_col += 1;
                } else if self.cursor_line < self.lines.len() - 1 {
                    self.cursor_line += 1;
                    self.cursor_col = 0;
                }
            }
        }
    }

    fn move_cursor_to(&mut self, line: usize, col: usize, extend_selection: bool) {
        if extend_selection && self.selection_start.is_none() {
            self.selection_start = Some((self.cursor_line, self.cursor_col));
        } else if !extend_selection {
            self.selection_start = None;
        }
        self.cursor_line = line.min(self.lines.len() - 1);
        self.cursor_col = col.min(self.lines[self.cursor_line].len());
    }

    /// Delete from cursor to the previous word boundary on the current line.
    fn delete_word_backward(&mut self) {
        if self.cursor_col > 0 {
            let line = &self.lines[self.cursor_line];
            let target = word_boundary_left(line, self.cursor_col);
            self.lines[self.cursor_line].drain(target..self.cursor_col);
            self.cursor_col = target;
            self.modified = true;
        } else if self.cursor_line > 0 {
            // At start of line, join with previous (same as delete_backward)
            let current_line = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].len();
            self.lines[self.cursor_line].push_str(&current_line);
            self.modified = true;
        }
    }

    /// Delete from cursor to the next word boundary on the current line.
    fn delete_word_forward(&mut self) {
        let line_len = self.current_line().len();
        if self.cursor_col < line_len {
            let line = &self.lines[self.cursor_line];
            let target = word_boundary_right(line, self.cursor_col);
            self.lines[self.cursor_line].drain(self.cursor_col..target);
            self.modified = true;
        } else if self.cursor_line < self.lines.len() - 1 {
            // At end of line, join with next (same as delete_forward)
            let next_line = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].push_str(&next_line);
            self.modified = true;
        }
    }

    fn ensure_cursor_visible(&mut self, visible_lines: usize) {
        if self.cursor_line < self.scroll_offset {
            self.scroll_offset = self.cursor_line;
        } else if self.cursor_line >= self.scroll_offset + visible_lines {
            self.scroll_offset = self.cursor_line - visible_lines + 1;
        }
    }

    /// Update cursor blink state. Call once per frame.
    pub fn update_cursor(&mut self, now: std::time::Instant) {
        if self.state.focused {
            if now.duration_since(self.last_blink).as_millis() >= 530 {
                self.cursor_visible = !self.cursor_visible;
                self.last_blink = now;
            }
        } else {
            self.cursor_visible = true;
            self.last_blink = now;
        }
    }
}

impl Default for TextArea {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for TextArea {
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
                    // Calculate cursor position from click
                    let line_height = 24.0; // Approximate
                    let text_x = bounds.x + 8.0;
                    let text_y = bounds.y + 4.0;

                    let clicked_line = ((*y - text_y) / line_height) as usize + self.scroll_offset;
                    self.cursor_line = clicked_line.min(self.lines.len() - 1);

                    let char_width = 10.0;
                    let click_offset = (*x - text_x).max(0.0);
                    self.cursor_col = ((click_offset / char_width) as usize).min(self.current_line().len());

                    return EventResponse::Consumed;
                }
            }
            InputEvent::KeyDown { key, modifiers, text } if self.state.focused => {
                match key {
                    Key::Left if modifiers.ctrl => {
                        let line = &self.lines[self.cursor_line];
                        let target = word_boundary_left(line, self.cursor_col);
                        self.move_cursor_to(self.cursor_line, target, modifiers.shift);
                        self.cursor_visible = true;
                        self.last_blink = std::time::Instant::now();
                        return EventResponse::Consumed;
                    }
                    Key::Right if modifiers.ctrl => {
                        let line = &self.lines[self.cursor_line];
                        let target = word_boundary_right(line, self.cursor_col);
                        self.move_cursor_to(self.cursor_line, target, modifiers.shift);
                        self.cursor_visible = true;
                        self.last_blink = std::time::Instant::now();
                        return EventResponse::Consumed;
                    }
                    Key::Left => {
                        if modifiers.shift && self.selection_start.is_none() {
                            self.selection_start = Some((self.cursor_line, self.cursor_col));
                        } else if !modifiers.shift {
                            self.selection_start = None;
                        }
                        self.move_cursor(-1, 0);
                        return EventResponse::Consumed;
                    }
                    Key::Right => {
                        if modifiers.shift && self.selection_start.is_none() {
                            self.selection_start = Some((self.cursor_line, self.cursor_col));
                        } else if !modifiers.shift {
                            self.selection_start = None;
                        }
                        self.move_cursor(1, 0);
                        return EventResponse::Consumed;
                    }
                    Key::Up => {
                        if modifiers.shift && self.selection_start.is_none() {
                            self.selection_start = Some((self.cursor_line, self.cursor_col));
                        } else if !modifiers.shift {
                            self.selection_start = None;
                        }
                        self.move_cursor(0, -1);
                        let visible_lines = (bounds.height / 24.0) as usize;
                        self.ensure_cursor_visible(visible_lines);
                        return EventResponse::Consumed;
                    }
                    Key::Down => {
                        if modifiers.shift && self.selection_start.is_none() {
                            self.selection_start = Some((self.cursor_line, self.cursor_col));
                        } else if !modifiers.shift {
                            self.selection_start = None;
                        }
                        self.move_cursor(0, 1);
                        let visible_lines = (bounds.height / 24.0) as usize;
                        self.ensure_cursor_visible(visible_lines);
                        return EventResponse::Consumed;
                    }
                    Key::Home if modifiers.ctrl => {
                        // Ctrl+Home: move to start of document
                        self.move_cursor_to(0, 0, modifiers.shift);
                        let visible_lines = (bounds.height / 24.0) as usize;
                        self.ensure_cursor_visible(visible_lines);
                        return EventResponse::Consumed;
                    }
                    Key::End if modifiers.ctrl => {
                        // Ctrl+End: move to end of document
                        let last_line = self.lines.len() - 1;
                        let last_col = self.lines[last_line].len();
                        self.move_cursor_to(last_line, last_col, modifiers.shift);
                        let visible_lines = (bounds.height / 24.0) as usize;
                        self.ensure_cursor_visible(visible_lines);
                        return EventResponse::Consumed;
                    }
                    Key::Home => {
                        // Home: move to start of current line
                        self.move_cursor_to(self.cursor_line, 0, modifiers.shift);
                        return EventResponse::Consumed;
                    }
                    Key::End => {
                        // End: move to end of current line
                        let line_len = self.current_line().len();
                        self.move_cursor_to(self.cursor_line, line_len, modifiers.shift);
                        return EventResponse::Consumed;
                    }
                    Key::Enter => {
                        self.insert_newline();
                        let visible_lines = (bounds.height / 24.0) as usize;
                        self.ensure_cursor_visible(visible_lines);
                        self.cursor_visible = true;
                        self.last_blink = std::time::Instant::now();
                        return EventResponse::Consumed;
                    }
                    Key::Backspace if modifiers.ctrl => {
                        if self.selection_start.is_some() {
                            self.delete_selection();
                        } else {
                            self.delete_word_backward();
                        }
                        self.cursor_visible = true;
                        self.last_blink = std::time::Instant::now();
                        return EventResponse::Consumed;
                    }
                    Key::Delete if modifiers.ctrl => {
                        if self.selection_start.is_some() {
                            self.delete_selection();
                        } else {
                            self.delete_word_forward();
                        }
                        self.cursor_visible = true;
                        self.last_blink = std::time::Instant::now();
                        return EventResponse::Consumed;
                    }
                    Key::Backspace => {
                        if self.selection_start.is_some() {
                            self.delete_selection();
                        } else {
                            self.delete_backward();
                        }
                        self.cursor_visible = true;
                        self.last_blink = std::time::Instant::now();
                        return EventResponse::Consumed;
                    }
                    Key::Delete => {
                        if self.selection_start.is_some() {
                            self.delete_selection();
                        } else {
                            self.delete_forward();
                        }
                        self.cursor_visible = true;
                        self.last_blink = std::time::Instant::now();
                        return EventResponse::Consumed;
                    }
                    Key::Tab => {
                        // Insert 4 spaces
                        for _ in 0..4 {
                            self.insert_char(' ');
                        }
                        return EventResponse::Consumed;
                    }
                    Key::A if modifiers.only_ctrl() => {
                        // Select all
                        self.selection_start = Some((0, 0));
                        self.cursor_line = self.lines.len() - 1;
                        self.cursor_col = self.lines[self.cursor_line].len();
                        return EventResponse::Consumed;
                    }
                    Key::C if modifiers.only_ctrl() => {
                        // Copy selected text to clipboard
                        if let Some(text) = self.selected_text() {
                            if !text.is_empty() {
                                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                    let _ = clipboard.set_text(text);
                                }
                            }
                        }
                        return EventResponse::Consumed;
                    }
                    Key::V if modifiers.only_ctrl() => {
                        // Paste from clipboard (preserve newlines for multi-line)
                        if let Ok(mut clipboard) = arboard::Clipboard::new() {
                            if let Ok(pasted) = clipboard.get_text() {
                                if !pasted.is_empty() {
                                    self.delete_selection();
                                    // Insert pasted text, handling newlines
                                    for c in pasted.chars() {
                                        if c == '\n' || c == '\r' {
                                            self.insert_newline();
                                        } else if !c.is_control() {
                                            self.insert_char(c);
                                        }
                                    }
                                }
                            }
                        }
                        self.cursor_visible = true;
                        self.last_blink = std::time::Instant::now();
                        let visible_lines = (bounds.height / 24.0) as usize;
                        self.ensure_cursor_visible(visible_lines);
                        return EventResponse::Consumed;
                    }
                    Key::X if modifiers.only_ctrl() => {
                        // Cut selected text to clipboard
                        if let Some(text) = self.selected_text() {
                            if !text.is_empty() {
                                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                    let _ = clipboard.set_text(text);
                                }
                            }
                        }
                        self.delete_selection();
                        self.cursor_visible = true;
                        self.last_blink = std::time::Instant::now();
                        return EventResponse::Consumed;
                    }
                    _ if key.is_printable() && !modifiers.ctrl && !modifiers.alt => {
                        // Insert text from winit's logical key (handles keyboard layouts).
                        // This is the primary text insertion path on X11/Wayland where
                        // IME may not fire Ime::Commit for regular ASCII keypresses.
                        if let Some(t) = text {
                            for c in t.chars() {
                                if c == '\n' || c == '\r' {
                                    self.insert_newline();
                                } else if !c.is_control() {
                                    self.insert_char(c);
                                }
                            }
                            self.inserted_from_key = true;
                            self.cursor_visible = true;
                            self.last_blink = std::time::Instant::now();
                        }
                        return EventResponse::Consumed;
                    }
                    _ => {}
                }
            }
            InputEvent::TextInput(text) if self.state.focused => {
                // If we already inserted from the KeyDown event for this keystroke,
                // skip to avoid double-insertion.
                if self.inserted_from_key {
                    self.inserted_from_key = false;
                    return EventResponse::Consumed;
                }
                for c in text.chars() {
                    if c == '\n' || c == '\r' {
                        self.insert_newline();
                    } else if !c.is_control() {
                        self.insert_char(c);
                    }
                }
                self.cursor_visible = true;
                self.last_blink = std::time::Instant::now();
                return EventResponse::Consumed;
            }
            InputEvent::Scroll { delta_y, .. } if self.state.focused => {
                let scroll_lines = (delta_y / 10.0) as i32;
                if scroll_lines < 0 {
                    self.scroll_offset = self.scroll_offset.saturating_sub((-scroll_lines) as usize);
                } else {
                    self.scroll_offset = (self.scroll_offset + scroll_lines as usize)
                        .min(self.lines.len().saturating_sub(1));
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
        let bg_color = if self.state.focused {
            theme::SURFACE.lighten(0.02)
        } else {
            theme::SURFACE
        };
        output.spline_vertices.extend(create_rect_vertices(&bounds, bg_color.to_array()));

        // Border
        let border_color = if self.state.focused {
            theme::STATUS_AHEAD
        } else {
            theme::BORDER
        };
        output.spline_vertices.extend(create_rect_outline_vertices(
            &bounds,
            border_color.to_array(),
            1.0,
        ));

        let line_height = text_renderer.line_height();
        let text_x = bounds.x + 8.0;
        let text_y_start = bounds.y + 4.0;
        let visible_lines = ((bounds.height - 8.0) / line_height) as usize;

        // Draw visible lines
        for (i, line_idx) in (self.scroll_offset..self.lines.len())
            .take(visible_lines)
            .enumerate()
        {
            let y = text_y_start + i as f32 * line_height;
            let line = &self.lines[line_idx];

            if !line.is_empty() {
                output.text_vertices.extend(text_renderer.layout_text(
                    line,
                    text_x,
                    y,
                    theme::TEXT.to_array(),
                ));
            }

            // Draw cursor on this line (blinks when focused)
            if self.state.focused && self.cursor_visible && line_idx == self.cursor_line {
                let col = self.cursor_col.min(line.len());
                let cursor_x = text_x + text_renderer.measure_text(&line[..col]);
                let cursor_rect = Rect::new(cursor_x, y, 2.0, line_height);
                output.spline_vertices.extend(create_rect_vertices(
                    &cursor_rect,
                    theme::TEXT.to_array(),
                ));
            }
        }

        output
    }

    fn set_focused(&mut self, focused: bool) {
        self.state.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.state.focused
    }
}

