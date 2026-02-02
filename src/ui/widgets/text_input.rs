//! Single-line text input widget

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{create_rect_outline_vertices, create_rect_vertices, theme, Widget, WidgetId, WidgetOutput, WidgetState};
use crate::ui::{Rect, TextRenderer};

/// A single-line text input field
pub struct TextInput {
    id: WidgetId,
    state: WidgetState,
    /// The current text content
    pub text: String,
    /// Placeholder text shown when empty
    pub placeholder: String,
    /// Cursor position (character index)
    cursor: usize,
    /// Selection start (if any)
    selection_start: Option<usize>,
    /// Maximum length (0 = unlimited)
    pub max_length: usize,
    /// Whether the content was modified
    modified: bool,
}

impl TextInput {
    pub fn new() -> Self {
        Self {
            id: WidgetId::new(),
            state: WidgetState::new(),
            text: String::new(),
            placeholder: String::new(),
            cursor: 0,
            selection_start: None,
            max_length: 0,
            modified: false,
        }
    }

    pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    pub fn with_max_length(mut self, max_length: usize) -> Self {
        self.max_length = max_length;
        self
    }

    /// Set the text content
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.selection_start = None;
    }

    /// Check if the content was modified and clear the flag
    pub fn take_modified(&mut self) -> bool {
        let modified = self.modified;
        self.modified = false;
        modified
    }

    /// Get the current text
    pub fn text(&self) -> &str {
        &self.text
    }

    fn insert_char(&mut self, c: char) {
        if self.max_length > 0 && self.text.len() >= self.max_length {
            return;
        }

        // Delete selection if any
        self.delete_selection();

        self.text.insert(self.cursor, c);
        self.cursor += 1;
        self.modified = true;
    }

    fn delete_selection(&mut self) {
        if let Some(start) = self.selection_start {
            let (begin, end) = if start < self.cursor {
                (start, self.cursor)
            } else {
                (self.cursor, start)
            };
            self.text.drain(begin..end);
            self.cursor = begin;
            self.selection_start = None;
            self.modified = true;
        }
    }

    fn move_cursor(&mut self, delta: i32, extend_selection: bool) {
        if extend_selection && self.selection_start.is_none() {
            self.selection_start = Some(self.cursor);
        } else if !extend_selection {
            self.selection_start = None;
        }

        if delta < 0 {
            self.cursor = self.cursor.saturating_sub((-delta) as usize);
        } else {
            self.cursor = (self.cursor + delta as usize).min(self.text.len());
        }
    }
}

impl Default for TextInput {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for TextInput {
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
                    let text_x = bounds.x + 8.0;
                    let char_width = 10.0; // Rough estimate
                    let click_offset = (*x - text_x).max(0.0);
                    self.cursor = ((click_offset / char_width) as usize).min(self.text.len());
                    self.selection_start = None;
                    return EventResponse::Consumed;
                }
            }
            InputEvent::KeyDown { key, modifiers } if self.state.focused => {
                match key {
                    Key::Left => {
                        self.move_cursor(-1, modifiers.shift);
                        return EventResponse::Consumed;
                    }
                    Key::Right => {
                        self.move_cursor(1, modifiers.shift);
                        return EventResponse::Consumed;
                    }
                    Key::Home => {
                        if modifiers.shift && self.selection_start.is_none() {
                            self.selection_start = Some(self.cursor);
                        } else if !modifiers.shift {
                            self.selection_start = None;
                        }
                        self.cursor = 0;
                        return EventResponse::Consumed;
                    }
                    Key::End => {
                        if modifiers.shift && self.selection_start.is_none() {
                            self.selection_start = Some(self.cursor);
                        } else if !modifiers.shift {
                            self.selection_start = None;
                        }
                        self.cursor = self.text.len();
                        return EventResponse::Consumed;
                    }
                    Key::Backspace => {
                        if self.selection_start.is_some() {
                            self.delete_selection();
                        } else if self.cursor > 0 {
                            self.cursor -= 1;
                            self.text.remove(self.cursor);
                            self.modified = true;
                        }
                        return EventResponse::Consumed;
                    }
                    Key::Delete => {
                        if self.selection_start.is_some() {
                            self.delete_selection();
                        } else if self.cursor < self.text.len() {
                            self.text.remove(self.cursor);
                            self.modified = true;
                        }
                        return EventResponse::Consumed;
                    }
                    Key::A if modifiers.only_ctrl() => {
                        // Select all
                        self.selection_start = Some(0);
                        self.cursor = self.text.len();
                        return EventResponse::Consumed;
                    }
                    Key::C if modifiers.only_ctrl() => {
                        // Copy - would need clipboard integration
                        return EventResponse::Consumed;
                    }
                    Key::V if modifiers.only_ctrl() => {
                        // Paste - would need clipboard integration
                        return EventResponse::Consumed;
                    }
                    Key::X if modifiers.only_ctrl() => {
                        // Cut - would need clipboard integration
                        self.delete_selection();
                        return EventResponse::Consumed;
                    }
                    key if key.is_printable() && !modifiers.ctrl && !modifiers.alt => {
                        // Convert key to character
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
                    if !c.is_control() {
                        self.insert_char(c);
                    }
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
        let text_y = bounds.y + (bounds.height - line_height) / 2.0;
        let text_x = bounds.x + 8.0;

        // Text content or placeholder
        if self.text.is_empty() {
            output.text_vertices.extend(text_renderer.layout_text(
                &self.placeholder,
                text_x,
                text_y,
                theme::TEXT_MUTED.to_array(),
            ));
        } else {
            output.text_vertices.extend(text_renderer.layout_text(
                &self.text,
                text_x,
                text_y,
                theme::TEXT.to_array(),
            ));
        }

        // Cursor (when focused)
        if self.state.focused {
            let char_width = text_renderer.char_width();
            let cursor_x = text_x + self.cursor as f32 * char_width;
            let cursor_rect = Rect::new(cursor_x, bounds.y + 4.0, 2.0, bounds.height - 8.0);
            output.spline_vertices.extend(create_rect_vertices(
                &cursor_rect,
                theme::TEXT.to_array(),
            ));
        }

        // Character count (for max_length)
        if self.max_length > 0 {
            let count_text = format!("{}", self.text.len());
            let count_x = bounds.right() - text_renderer.measure_text(&count_text) - 8.0;
            output.text_vertices.extend(text_renderer.layout_text(
                &count_text,
                count_x,
                text_y,
                theme::TEXT_MUTED.to_array(),
            ));
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
