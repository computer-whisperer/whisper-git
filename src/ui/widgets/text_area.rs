//! Multi-line text area widget

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{create_rect_outline_vertices, create_rect_vertices, theme, Widget, WidgetId, WidgetOutput, WidgetState};
use crate::ui::{Rect, TextRenderer};

/// A multi-line text editing area
pub struct TextArea {
    id: WidgetId,
    state: WidgetState,
    /// Lines of text
    lines: Vec<String>,
    /// Cursor position (line, column)
    cursor_line: usize,
    cursor_col: usize,
    /// Scroll offset in lines
    scroll_offset: usize,
    /// Whether the content was modified
    modified: bool,
}

impl TextArea {
    pub fn new() -> Self {
        Self {
            id: WidgetId::new(),
            state: WidgetState::new(),
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
            scroll_offset: 0,
            modified: false,
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
        self.scroll_offset = 0;
    }

    /// Get the full text content
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Check if the content was modified and clear the flag
    pub fn take_modified(&mut self) -> bool {
        let modified = self.modified;
        self.modified = false;
        modified
    }

    fn current_line(&self) -> &String {
        &self.lines[self.cursor_line]
    }

    fn current_line_mut(&mut self) -> &mut String {
        &mut self.lines[self.cursor_line]
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

    fn ensure_cursor_visible(&mut self, visible_lines: usize) {
        if self.cursor_line < self.scroll_offset {
            self.scroll_offset = self.cursor_line;
        } else if self.cursor_line >= self.scroll_offset + visible_lines {
            self.scroll_offset = self.cursor_line - visible_lines + 1;
        }
    }
}

impl Default for TextArea {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for TextArea {
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
            InputEvent::KeyDown { key, modifiers } if self.state.focused => {
                match key {
                    Key::Left => {
                        self.move_cursor(-1, 0);
                        return EventResponse::Consumed;
                    }
                    Key::Right => {
                        self.move_cursor(1, 0);
                        return EventResponse::Consumed;
                    }
                    Key::Up => {
                        self.move_cursor(0, -1);
                        let visible_lines = (bounds.height / 24.0) as usize;
                        self.ensure_cursor_visible(visible_lines);
                        return EventResponse::Consumed;
                    }
                    Key::Down => {
                        self.move_cursor(0, 1);
                        let visible_lines = (bounds.height / 24.0) as usize;
                        self.ensure_cursor_visible(visible_lines);
                        return EventResponse::Consumed;
                    }
                    Key::Home => {
                        self.cursor_col = 0;
                        return EventResponse::Consumed;
                    }
                    Key::End => {
                        self.cursor_col = self.current_line().len();
                        return EventResponse::Consumed;
                    }
                    Key::Enter => {
                        self.insert_newline();
                        let visible_lines = (bounds.height / 24.0) as usize;
                        self.ensure_cursor_visible(visible_lines);
                        return EventResponse::Consumed;
                    }
                    Key::Backspace => {
                        self.delete_backward();
                        return EventResponse::Consumed;
                    }
                    Key::Delete => {
                        self.delete_forward();
                        return EventResponse::Consumed;
                    }
                    Key::Tab => {
                        // Insert 4 spaces
                        for _ in 0..4 {
                            self.insert_char(' ');
                        }
                        return EventResponse::Consumed;
                    }
                    key if key.is_printable() && !modifiers.ctrl && !modifiers.alt => {
                        if let Some(c) = key_to_char(*key, modifiers.shift) {
                            self.insert_char(c);
                            return EventResponse::Consumed;
                        }
                    }
                    _ => {}
                }
            }
            InputEvent::TextInput(text) if self.state.focused => {
                for c in text.chars() {
                    if c == '\n' || c == '\r' {
                        self.insert_newline();
                    } else if !c.is_control() {
                        self.insert_char(c);
                    }
                }
                return EventResponse::Consumed;
            }
            InputEvent::Scroll { delta_y, .. } if self.state.focused => {
                let scroll_lines = (delta_y / 20.0) as i32;
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

            // Draw cursor on this line
            if self.state.focused && line_idx == self.cursor_line {
                let cursor_x = text_x + self.cursor_col as f32 * text_renderer.char_width();
                let cursor_rect = Rect::new(cursor_x, y, 2.0, line_height);
                output.spline_vertices.extend(create_rect_vertices(
                    &cursor_rect,
                    theme::TEXT.to_array(),
                ));
            }
        }

        output
    }

    fn focusable(&self) -> bool {
        self.state.enabled
    }

    fn set_focused(&mut self, focused: bool) {
        self.state.focused = focused;
    }
}

/// Convert a key to a character, considering shift state
fn key_to_char(key: Key, shift: bool) -> Option<char> {
    match key {
        Key::A => Some(if shift { 'A' } else { 'a' }),
        Key::B => Some(if shift { 'B' } else { 'b' }),
        Key::C => Some(if shift { 'C' } else { 'c' }),
        Key::D => Some(if shift { 'D' } else { 'd' }),
        Key::E => Some(if shift { 'E' } else { 'e' }),
        Key::F => Some(if shift { 'F' } else { 'f' }),
        Key::G => Some(if shift { 'G' } else { 'g' }),
        Key::H => Some(if shift { 'H' } else { 'h' }),
        Key::I => Some(if shift { 'I' } else { 'i' }),
        Key::J => Some(if shift { 'J' } else { 'j' }),
        Key::K => Some(if shift { 'K' } else { 'k' }),
        Key::L => Some(if shift { 'L' } else { 'l' }),
        Key::M => Some(if shift { 'M' } else { 'm' }),
        Key::N => Some(if shift { 'N' } else { 'n' }),
        Key::O => Some(if shift { 'O' } else { 'o' }),
        Key::P => Some(if shift { 'P' } else { 'p' }),
        Key::Q => Some(if shift { 'Q' } else { 'q' }),
        Key::R => Some(if shift { 'R' } else { 'r' }),
        Key::S => Some(if shift { 'S' } else { 's' }),
        Key::T => Some(if shift { 'T' } else { 't' }),
        Key::U => Some(if shift { 'U' } else { 'u' }),
        Key::V => Some(if shift { 'V' } else { 'v' }),
        Key::W => Some(if shift { 'W' } else { 'w' }),
        Key::X => Some(if shift { 'X' } else { 'x' }),
        Key::Y => Some(if shift { 'Y' } else { 'y' }),
        Key::Z => Some(if shift { 'Z' } else { 'z' }),
        Key::Num0 => Some(if shift { ')' } else { '0' }),
        Key::Num1 => Some(if shift { '!' } else { '1' }),
        Key::Num2 => Some(if shift { '@' } else { '2' }),
        Key::Num3 => Some(if shift { '#' } else { '3' }),
        Key::Num4 => Some(if shift { '$' } else { '4' }),
        Key::Num5 => Some(if shift { '%' } else { '5' }),
        Key::Num6 => Some(if shift { '^' } else { '6' }),
        Key::Num7 => Some(if shift { '&' } else { '7' }),
        Key::Num8 => Some(if shift { '*' } else { '8' }),
        Key::Num9 => Some(if shift { '(' } else { '9' }),
        Key::Space => Some(' '),
        Key::Minus => Some(if shift { '_' } else { '-' }),
        Key::Equals => Some(if shift { '+' } else { '=' }),
        Key::LeftBracket => Some(if shift { '{' } else { '[' }),
        Key::RightBracket => Some(if shift { '}' } else { ']' }),
        Key::Backslash => Some(if shift { '|' } else { '\\' }),
        Key::Semicolon => Some(if shift { ':' } else { ';' }),
        Key::Quote => Some(if shift { '"' } else { '\'' }),
        Key::Comma => Some(if shift { '<' } else { ',' }),
        Key::Period => Some(if shift { '>' } else { '.' }),
        Key::Slash => Some(if shift { '?' } else { '/' }),
        Key::Grave => Some(if shift { '~' } else { '`' }),
        _ => None,
    }
}
