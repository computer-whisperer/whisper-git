//! Single-line text input widget

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{create_rect_outline_vertices, create_rect_vertices, create_rounded_rect_vertices, theme, Widget, WidgetOutput, WidgetState};
use crate::ui::{Rect, TextRenderer};

/// A single-line text input field
pub struct TextInput {
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
    /// Guard against double-insertion: set when KeyDown inserts text,
    /// cleared when TextInput fires for the same keystroke
    inserted_from_key: bool,
    /// Whether the cursor is currently visible (for blinking)
    cursor_visible: bool,
    /// Last time the cursor blink state changed
    last_blink: std::time::Instant,
}

impl TextInput {
    pub fn new() -> Self {
        Self {
            state: WidgetState::new(),
            text: String::new(),
            placeholder: String::new(),
            cursor: 0,
            selection_start: None,
            max_length: 0,
            modified: false,
            inserted_from_key: false,
            cursor_visible: true,
            last_blink: std::time::Instant::now(),
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

    /// Get the current text
    pub fn text(&self) -> &str {
        &self.text
    }

    fn insert_char(&mut self, c: char) {
        if self.max_length > 0 && self.text.chars().count() >= self.max_length {
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
            InputEvent::KeyDown { key, modifiers, text } if self.state.focused => {
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
                        self.cursor_visible = true;
                        self.last_blink = std::time::Instant::now();
                        return EventResponse::Consumed;
                    }
                    Key::Delete => {
                        if self.selection_start.is_some() {
                            self.delete_selection();
                        } else if self.cursor < self.text.len() {
                            self.text.remove(self.cursor);
                            self.modified = true;
                        }
                        self.cursor_visible = true;
                        self.last_blink = std::time::Instant::now();
                        return EventResponse::Consumed;
                    }
                    Key::A if modifiers.only_ctrl() => {
                        // Select all
                        self.selection_start = Some(0);
                        self.cursor = self.text.len();
                        return EventResponse::Consumed;
                    }
                    Key::C if modifiers.only_ctrl() => {
                        // Copy selected text to clipboard
                        if let Some(sel_start) = self.selection_start {
                            let (begin, end) = if sel_start < self.cursor {
                                (sel_start, self.cursor)
                            } else {
                                (self.cursor, sel_start)
                            };
                            let selected = &self.text[begin..end];
                            if !selected.is_empty() {
                                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                    let _ = clipboard.set_text(selected);
                                }
                            }
                        }
                        return EventResponse::Consumed;
                    }
                    Key::V if modifiers.only_ctrl() => {
                        // Paste from clipboard (strip newlines for single-line input)
                        if let Ok(mut clipboard) = arboard::Clipboard::new() {
                            if let Ok(pasted) = clipboard.get_text() {
                                // Strip newlines/carriage returns for single-line input
                                let clean: String = pasted.chars()
                                    .filter(|c| *c != '\n' && *c != '\r')
                                    .collect();
                                if !clean.is_empty() {
                                    self.delete_selection();
                                    self.text.insert_str(self.cursor, &clean);
                                    self.cursor += clean.len();
                                    self.modified = true;
                                }
                            }
                        }
                        self.cursor_visible = true;
                        self.last_blink = std::time::Instant::now();
                        return EventResponse::Consumed;
                    }
                    Key::X if modifiers.only_ctrl() => {
                        // Cut selected text to clipboard
                        if let Some(sel_start) = self.selection_start {
                            let (begin, end) = if sel_start < self.cursor {
                                (sel_start, self.cursor)
                            } else {
                                (self.cursor, sel_start)
                            };
                            let selected = &self.text[begin..end];
                            if !selected.is_empty() {
                                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                    let _ = clipboard.set_text(selected);
                                }
                            }
                        }
                        self.delete_selection();
                        return EventResponse::Consumed;
                    }
                    _ if key.is_printable() && !modifiers.ctrl && !modifiers.alt => {
                        // Insert text from winit's logical key (handles keyboard layouts).
                        // This is the primary text insertion path on X11/Wayland where
                        // IME may not fire Ime::Commit for regular ASCII keypresses.
                        if let Some(t) = text {
                            for c in t.chars() {
                                if !c.is_control() {
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
                    if !c.is_control() {
                        self.insert_char(c);
                    }
                }
                self.cursor_visible = true;
                self.last_blink = std::time::Instant::now();
                return EventResponse::Consumed;
            }
            _ => {}
        }

        EventResponse::Ignored
    }

    fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();
        let padding = 12.0;
        let corner_radius = (bounds.height * 0.15).min(5.0);

        // Background - slightly raised when focused
        let bg_color = if self.state.focused {
            theme::SURFACE_RAISED
        } else {
            theme::SURFACE
        };
        output.spline_vertices.extend(create_rounded_rect_vertices(&bounds, bg_color.to_array(), corner_radius));

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
        let text_y = bounds.y + (bounds.height - line_height) / 2.0;
        let text_x = bounds.x + padding;

        // Text content or placeholder
        if self.text.is_empty() {
            output.text_vertices.extend(text_renderer.layout_text(
                &self.placeholder,
                text_x,
                text_y,
                theme::TEXT_MUTED.to_array(),
            ));
        } else {
            // Use bright text when focused
            let text_color = if self.state.focused {
                theme::TEXT_BRIGHT
            } else {
                theme::TEXT
            };
            output.text_vertices.extend(text_renderer.layout_text(
                &self.text,
                text_x,
                text_y,
                text_color.to_array(),
            ));
        }

        // Cursor (when focused and visible per blink cycle)
        if self.state.focused && self.cursor_visible {
            let cursor_x = text_x + text_renderer.measure_text(&self.text[..self.cursor]);
            let cursor_rect = Rect::new(cursor_x, bounds.y + 6.0, 2.0, bounds.height - 12.0);
            output.spline_vertices.extend(create_rect_vertices(
                &cursor_rect,
                theme::ACCENT.to_array(),
            ));
        }

        // Character count (for max_length) - show in corner
        if self.max_length > 0 {
            let count_text = format!("{}", self.text.chars().count());
            let count_x = bounds.right() - text_renderer.measure_text(&count_text) - padding;
            output.text_vertices.extend(text_renderer.layout_text(
                &count_text,
                count_x,
                text_y,
                theme::TEXT_MUTED.to_array(),
            ));
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

