//! Search bar widget - text input with search icon and clear button

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{create_rect_vertices, create_rect_outline_vertices, theme, WidgetOutput};
use crate::ui::{Rect, TextRenderer};

/// Actions produced by the search bar
#[derive(Clone, Debug)]
pub enum SearchAction {
    /// The search query changed
    QueryChanged(String),
    /// The search bar was closed
    Closed,
}

/// A search/filter bar with text input, search icon, and clear button
pub struct SearchBar {
    /// Current query text
    query: String,
    /// Whether the search bar is active/visible
    active: bool,
    /// Cursor position in the query
    cursor: usize,
    /// Number of matches found
    match_count: usize,
    /// Current match index (for cycling)
    current_match: usize,
    /// Pending action
    pending_action: Option<SearchAction>,
}

impl SearchBar {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            active: false,
            cursor: 0,
            match_count: 0,
            current_match: 0,
            pending_action: None,
        }
    }

    /// Get the current query
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Whether the search bar is currently active
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Activate the search bar
    pub fn activate(&mut self) {
        self.active = true;
    }

    /// Deactivate and clear
    pub fn deactivate(&mut self) {
        self.active = false;
        self.query.clear();
        self.cursor = 0;
        self.match_count = 0;
        self.current_match = 0;
        self.pending_action = Some(SearchAction::Closed);
    }

    /// Set the match count (called from parent after filtering)
    pub fn set_match_count(&mut self, count: usize) {
        self.match_count = count;
        if self.current_match >= count && count > 0 {
            self.current_match = 0;
        }
    }

    /// Get current match index
    pub fn current_match(&self) -> usize {
        self.current_match
    }

    /// Cycle to next match
    pub fn next_match(&mut self) {
        if self.match_count > 0 {
            self.current_match = (self.current_match + 1) % self.match_count;
        }
    }

    /// Cycle to previous match
    pub fn prev_match(&mut self) {
        if self.match_count > 0 {
            self.current_match = if self.current_match == 0 {
                self.match_count - 1
            } else {
                self.current_match - 1
            };
        }
    }

    /// Take the pending action
    pub fn take_action(&mut self) -> Option<SearchAction> {
        self.pending_action.take()
    }

    /// Handle input events
    pub fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        if !self.active {
            return EventResponse::Ignored;
        }

        match event {
            InputEvent::KeyDown { key, modifiers } => {
                match key {
                    Key::Escape => {
                        self.deactivate();
                        return EventResponse::Consumed;
                    }
                    Key::Enter => {
                        if modifiers.shift {
                            self.prev_match();
                        } else {
                            self.next_match();
                        }
                        return EventResponse::Consumed;
                    }
                    Key::Backspace => {
                        if self.cursor > 0 {
                            self.cursor -= 1;
                            self.query.remove(self.cursor);
                            self.pending_action = Some(SearchAction::QueryChanged(self.query.clone()));
                        }
                        return EventResponse::Consumed;
                    }
                    Key::Delete => {
                        if self.cursor < self.query.len() {
                            self.query.remove(self.cursor);
                            self.pending_action = Some(SearchAction::QueryChanged(self.query.clone()));
                        }
                        return EventResponse::Consumed;
                    }
                    Key::Left => {
                        self.cursor = self.cursor.saturating_sub(1);
                        return EventResponse::Consumed;
                    }
                    Key::Right => {
                        self.cursor = (self.cursor + 1).min(self.query.len());
                        return EventResponse::Consumed;
                    }
                    Key::Home => {
                        self.cursor = 0;
                        return EventResponse::Consumed;
                    }
                    Key::End => {
                        self.cursor = self.query.len();
                        return EventResponse::Consumed;
                    }
                    Key::A if modifiers.only_ctrl() => {
                        // Select all - just move cursor to end
                        self.cursor = self.query.len();
                        return EventResponse::Consumed;
                    }
                    key if key.is_printable() && !modifiers.ctrl && !modifiers.alt => {
                        if let Some(c) = key_to_char(*key, modifiers.shift) {
                            self.query.insert(self.cursor, c);
                            self.cursor += 1;
                            self.pending_action = Some(SearchAction::QueryChanged(self.query.clone()));
                            return EventResponse::Consumed;
                        }
                    }
                    _ => {}
                }
            }
            InputEvent::TextInput(text) => {
                for c in text.chars() {
                    if !c.is_control() {
                        self.query.insert(self.cursor, c);
                        self.cursor += 1;
                    }
                }
                if !text.is_empty() {
                    self.pending_action = Some(SearchAction::QueryChanged(self.query.clone()));
                    return EventResponse::Consumed;
                }
            }
            InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } => {
                if bounds.contains(*x, *y) {
                    // Check if clicking the clear button (right edge)
                    let clear_rect = Rect::new(
                        bounds.right() - 24.0,
                        bounds.y,
                        24.0,
                        bounds.height,
                    );
                    if clear_rect.contains(*x, *y) && !self.query.is_empty() {
                        self.query.clear();
                        self.cursor = 0;
                        self.pending_action = Some(SearchAction::QueryChanged(String::new()));
                        return EventResponse::Consumed;
                    }
                    return EventResponse::Consumed;
                }
            }
            _ => {}
        }

        EventResponse::Ignored
    }

    /// Render the search bar
    pub fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        if !self.active {
            return output;
        }

        let line_height = text_renderer.line_height();
        let padding = 8.0;

        // Background
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            theme::SURFACE_RAISED.to_array(),
        ));

        // Border
        output.spline_vertices.extend(create_rect_outline_vertices(
            &bounds,
            theme::ACCENT.to_array(),
            1.0,
        ));

        let text_y = bounds.y + (bounds.height - line_height) / 2.0;

        // Search icon (magnifying glass as "?" text)
        let icon_text = "?";
        output.text_vertices.extend(text_renderer.layout_text(
            icon_text,
            bounds.x + padding,
            text_y,
            theme::TEXT_MUTED.to_array(),
        ));

        let icon_width = text_renderer.measure_text(icon_text) + padding;
        let text_x = bounds.x + padding + icon_width;

        // Query text or placeholder
        if self.query.is_empty() {
            output.text_vertices.extend(text_renderer.layout_text(
                "Search commits...",
                text_x,
                text_y,
                theme::TEXT_MUTED.with_alpha(0.5).to_array(),
            ));
        } else {
            output.text_vertices.extend(text_renderer.layout_text(
                &self.query,
                text_x,
                text_y,
                theme::TEXT_BRIGHT.to_array(),
            ));
        }

        // Cursor
        let char_width = text_renderer.char_width();
        let cursor_x = text_x + self.cursor as f32 * char_width;
        let cursor_rect = Rect::new(cursor_x, bounds.y + 4.0, 2.0, bounds.height - 8.0);
        output.spline_vertices.extend(create_rect_vertices(
            &cursor_rect,
            theme::ACCENT.to_array(),
        ));

        // Match count on the right
        let right_x = bounds.right() - padding;
        if !self.query.is_empty() {
            let count_text = if self.match_count == 0 {
                "No matches".to_string()
            } else {
                format!("{}/{}", self.current_match + 1, self.match_count)
            };
            let count_width = text_renderer.measure_text(&count_text);
            let count_color = if self.match_count == 0 {
                theme::STATUS_DIRTY.to_array()
            } else {
                theme::TEXT_MUTED.to_array()
            };
            output.text_vertices.extend(text_renderer.layout_text(
                &count_text,
                right_x - count_width - 28.0,
                text_y,
                count_color,
            ));
        }

        // Clear button "x" on the right
        if !self.query.is_empty() {
            let clear_text = "x";
            let clear_width = text_renderer.measure_text(clear_text);
            output.text_vertices.extend(text_renderer.layout_text(
                clear_text,
                right_x - clear_width - 4.0,
                text_y,
                theme::TEXT_MUTED.to_array(),
            ));
        }

        output
    }
}

impl Default for SearchBar {
    fn default() -> Self {
        Self::new()
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
