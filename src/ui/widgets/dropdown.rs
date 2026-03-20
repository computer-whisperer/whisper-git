//! Dropdown selector widget - a closed display with a popup option list

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{
    Widget, WidgetOutput, WidgetState, create_rect_outline_vertices, create_rect_vertices,
    create_rounded_rect_vertices, theme,
};
use crate::ui::{Rect, TextRenderer};

/// A dropdown selector that displays the current selection and opens a popup list.
pub struct Dropdown {
    state: WidgetState,
    /// Available options
    options: Vec<String>,
    /// Currently selected index
    selected: usize,
    /// Whether the popup list is open
    open: bool,
    /// Hovered item index in the popup (None if nothing hovered)
    hovered_index: Option<usize>,
    /// Placeholder text when no options exist
    placeholder: String,
    /// Cached bounds of the closed control (set during layout, used for popup positioning)
    closed_bounds: Rect,
}

impl Dropdown {
    pub fn new() -> Self {
        Self {
            state: WidgetState::new(),
            options: Vec::new(),
            selected: 0,
            open: false,
            hovered_index: None,
            placeholder: String::new(),
            closed_bounds: Rect::new(0.0, 0.0, 0.0, 0.0),
        }
    }

    pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Set the available options and select the one matching `selected_value`,
    /// or index 0 if not found.
    pub fn set_options(&mut self, options: Vec<String>, selected_value: &str) {
        self.selected = options
            .iter()
            .position(|o| o == selected_value)
            .unwrap_or(0);
        self.options = options;
        self.open = false;
        self.hovered_index = None;
    }

    /// Get the currently selected value (empty string if no options).
    pub fn selected_value(&self) -> &str {
        self.options
            .get(self.selected)
            .map(|s| s.as_str())
            .unwrap_or("")
    }

    /// Whether the popup is currently open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    fn item_height(&self) -> f32 {
        28.0
    }

    fn popup_height(&self) -> f32 {
        let count = self.options.len().min(8) as f32; // max 8 visible
        count * self.item_height() + 4.0 // 2px padding top+bottom
    }

    /// Bounds of the popup list, positioned below the closed control.
    fn popup_bounds(&self) -> Rect {
        Rect::new(
            self.closed_bounds.x,
            self.closed_bounds.bottom(),
            self.closed_bounds.width,
            self.popup_height(),
        )
    }

    fn item_index_at_y(&self, rel_y: f32) -> Option<usize> {
        let y = rel_y - 2.0; // top padding
        if y < 0.0 {
            return None;
        }
        let idx = (y / self.item_height()) as usize;
        if idx < self.options.len() {
            Some(idx)
        } else {
            None
        }
    }

    fn select_index(&mut self, idx: usize) {
        if idx < self.options.len() {
            self.selected = idx;
        }
        self.open = false;
        self.hovered_index = None;
    }
}

impl Default for Dropdown {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for Dropdown {
    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        self.closed_bounds = bounds;

        if self.open {
            let popup = self.popup_bounds();

            match event {
                InputEvent::MouseMove { x, y, .. } => {
                    if popup.contains(*x, *y) {
                        let rel_y = *y - popup.y;
                        self.hovered_index = self.item_index_at_y(rel_y);
                    } else {
                        self.hovered_index = None;
                    }
                    return EventResponse::Consumed;
                }
                InputEvent::MouseDown {
                    button: MouseButton::Left,
                    x,
                    y,
                    ..
                } => {
                    if popup.contains(*x, *y) {
                        let rel_y = *y - popup.y;
                        if let Some(idx) = self.item_index_at_y(rel_y) {
                            self.select_index(idx);
                        }
                    } else if bounds.contains(*x, *y) {
                        // Clicked the closed control while open — toggle closed
                        self.open = false;
                        self.hovered_index = None;
                    } else {
                        // Click outside — close
                        self.open = false;
                        self.hovered_index = None;
                    }
                    return EventResponse::Consumed;
                }
                InputEvent::KeyDown { key, .. } => match key {
                    Key::Escape => {
                        self.open = false;
                        self.hovered_index = None;
                        return EventResponse::Consumed;
                    }
                    Key::Enter | Key::Space => {
                        if let Some(idx) = self.hovered_index {
                            self.select_index(idx);
                        } else {
                            self.open = false;
                        }
                        return EventResponse::Consumed;
                    }
                    Key::Down | Key::J => {
                        let next = self
                            .hovered_index
                            .map(|i| (i + 1).min(self.options.len().saturating_sub(1)))
                            .or(Some(0));
                        self.hovered_index = next;
                        return EventResponse::Consumed;
                    }
                    Key::Up | Key::K => {
                        let prev = self.hovered_index.map(|i| i.saturating_sub(1)).or(Some(0));
                        self.hovered_index = prev;
                        return EventResponse::Consumed;
                    }
                    _ => {
                        return EventResponse::Consumed;
                    }
                },
                InputEvent::Scroll { .. } | InputEvent::MouseUp { .. } => {
                    return EventResponse::Consumed;
                }
                _ => {}
            }

            return EventResponse::Consumed;
        }

        // Closed state
        match event {
            InputEvent::MouseDown {
                button: MouseButton::Left,
                x,
                y,
                ..
            } => {
                if bounds.contains(*x, *y) {
                    self.state.focused = true;
                    if !self.options.is_empty() {
                        self.open = true;
                        self.hovered_index = Some(self.selected);
                    }
                    return EventResponse::Consumed;
                }
            }
            InputEvent::KeyDown {
                key: Key::Enter | Key::Space | Key::Down,
                ..
            } if self.state.focused => {
                if !self.options.is_empty() {
                    self.open = true;
                    self.hovered_index = Some(self.selected);
                }
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

        // Background
        let bg_color = if self.state.focused {
            theme::SURFACE_RAISED
        } else {
            theme::SURFACE
        };
        output.spline_vertices.extend(create_rounded_rect_vertices(
            &bounds,
            bg_color.to_array(),
            corner_radius,
        ));

        // Border
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

        // Display selected value or placeholder
        let display_text = self.selected_value();
        if display_text.is_empty() {
            output.text_vertices.extend(text_renderer.layout_text(
                &self.placeholder,
                text_x,
                text_y,
                theme::TEXT_MUTED.to_array(),
            ));
        } else {
            let text_color = if self.state.focused {
                theme::TEXT_BRIGHT
            } else {
                theme::TEXT
            };
            output.text_vertices.extend(text_renderer.layout_text(
                display_text,
                text_x,
                text_y,
                text_color.to_array(),
            ));
        }

        // Down arrow indicator on the right
        let arrow_text = "\u{25BE}"; // ▾ small down triangle
        let arrow_w = text_renderer.measure_text(arrow_text);
        let arrow_x = bounds.right() - padding - arrow_w;
        output.text_vertices.extend(text_renderer.layout_text(
            arrow_text,
            arrow_x,
            text_y,
            theme::TEXT_MUTED.to_array(),
        ));

        output
    }

    fn set_focused(&mut self, focused: bool) {
        self.state.focused = focused;
        if !focused {
            self.open = false;
            self.hovered_index = None;
        }
    }

    fn is_focused(&self) -> bool {
        self.state.focused
    }
}

impl Dropdown {
    /// Render the popup overlay separately so it draws on top of all dialog content.
    /// Must be rendered in a later render pass than the dialog that contains this dropdown.
    pub fn layout_popup(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        if !self.open || self.options.is_empty() {
            return output;
        }

        let padding = 12.0;
        let line_height = text_renderer.line_height();
        let popup = Rect::new(bounds.x, bounds.bottom(), bounds.width, self.popup_height());
        let popup_radius = 4.0;

        // Shadow
        let shadow = Rect::new(popup.x + 2.0, popup.y + 2.0, popup.width, popup.height);
        output.spline_vertices.extend(create_rounded_rect_vertices(
            &shadow,
            [0.0, 0.0, 0.0, 0.4],
            popup_radius,
        ));

        // Background
        output.spline_vertices.extend(create_rounded_rect_vertices(
            &popup,
            theme::SURFACE_RAISED.lighten(0.02).to_array(),
            popup_radius,
        ));

        // Border
        output.spline_vertices.extend(create_rect_outline_vertices(
            &popup,
            theme::BORDER_LIGHT.to_array(),
            1.0,
        ));

        // Items
        let item_h = self.item_height();
        for (idx, option) in self.options.iter().enumerate() {
            let item_y = popup.y + 2.0 + idx as f32 * item_h;
            let item_rect = Rect::new(popup.x + 1.0, item_y, popup.width - 2.0, item_h);

            // Highlight: hovered or selected
            if self.hovered_index == Some(idx) {
                output.spline_vertices.extend(create_rect_vertices(
                    &item_rect,
                    theme::ACCENT.with_alpha(0.25).to_array(),
                ));
            } else if idx == self.selected {
                output.spline_vertices.extend(create_rect_vertices(
                    &item_rect,
                    theme::ACCENT.with_alpha(0.10).to_array(),
                ));
            }

            let item_text_y = item_y + (item_h - line_height) / 2.0;
            let text_color = if self.hovered_index == Some(idx) {
                theme::TEXT_BRIGHT
            } else {
                theme::TEXT
            };
            output.text_vertices.extend(text_renderer.layout_text(
                option,
                popup.x + padding,
                item_text_y,
                text_color.to_array(),
            ));
        }

        output
    }
}
