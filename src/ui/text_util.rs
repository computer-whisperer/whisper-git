//! Shared text utility functions used across multiple views.

use crate::ui::TextRenderer;

/// Truncate text to fit within `max_width` pixels, appending "..." if needed.
///
/// Uses the provided `TextRenderer` to measure glyph widths. Returns the
/// original string when it already fits.
pub fn truncate_to_width(text: &str, text_renderer: &TextRenderer, max_width: f32) -> String {
    if max_width <= 0.0 {
        return String::new();
    }
    let full_width = text_renderer.measure_text(text);
    if full_width <= max_width {
        return text.to_string();
    }
    let ellipsis = "...";
    let ellipsis_width = text_renderer.measure_text(ellipsis);
    let target_width = max_width - ellipsis_width;
    if target_width <= 0.0 {
        return ellipsis.to_string();
    }
    let mut width = 0.0;
    let mut end = 0;
    for (i, c) in text.char_indices() {
        let cw = text_renderer.measure_text(&text[i..i + c.len_utf8()]);
        if width + cw > target_width {
            break;
        }
        width += cw;
        end = i + c.len_utf8();
    }
    format!("{}{}", &text[..end], ellipsis)
}

/// Word-wrap a message into lines that fit within `max_width`.
/// Returns a Vec of line strings.
pub fn wrap_text(message: &str, max_width: f32, text_renderer: &TextRenderer) -> Vec<String> {
    let mut lines = Vec::new();
    let words: Vec<&str> = message.split_whitespace().collect();
    if words.is_empty() {
        lines.push(String::new());
        return lines;
    }

    let space_width = text_renderer.measure_text(" ");
    let mut current_line = String::new();
    let mut current_width = 0.0_f32;

    for word in &words {
        let word_width = text_renderer.measure_text(word);

        if current_line.is_empty() {
            // First word on the line - always accept it even if it overflows
            current_line.push_str(word);
            current_width = word_width;
        } else if current_width + space_width + word_width <= max_width {
            // Fits on current line
            current_line.push(' ');
            current_line.push_str(word);
            current_width += space_width + word_width;
        } else {
            // Doesn't fit - start new line
            lines.push(current_line);
            current_line = word.to_string();
            current_width = word_width;
        }
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    lines
}

/// Clamp a scroll offset so it stays within `[0, max_scroll]`.
///
/// `content_height` is the total height of all content and `view_height` is
/// the visible viewport height.
pub fn clamp_scroll(scroll_offset: f32, content_height: f32, view_height: f32) -> f32 {
    let max_scroll = (content_height - view_height).max(0.0);
    scroll_offset.clamp(0.0, max_scroll)
}
