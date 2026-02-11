//! Button widget - clickable with visual states

use crate::input::{InputEvent, EventResponse, MouseButton};
use crate::ui::{Color, Rect, TextRenderer};
use crate::ui::widget::{Widget, WidgetState, WidgetOutput, create_rect_vertices, create_rounded_rect_vertices, create_rounded_rect_outline_vertices, theme};

/// A clickable button with text
pub struct Button {
    state: WidgetState,
    /// The button label
    pub label: String,
    /// Whether the button was just clicked
    clicked: bool,
    /// Normal background color
    pub background: Color,
    /// Hover background color
    pub hover_background: Color,
    /// Pressed background color
    pub pressed_background: Color,
    /// Text color
    pub text_color: Color,
    /// Border color
    pub border_color: Option<Color>,
}

impl Button {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            state: WidgetState::new(),
            label: label.into(),
            clicked: false,
            background: theme::SURFACE_RAISED,
            hover_background: Color::rgba(0.22, 0.22, 0.22, 1.0), // Noticeably lighter on hover
            pressed_background: theme::SURFACE,
            text_color: theme::TEXT,
            border_color: Some(theme::BORDER),
        }
    }

    /// Returns true if the button is currently in hovered state
    pub fn is_hovered(&self) -> bool {
        self.state.hovered
    }

    /// Check if the button was clicked this frame (and clear the flag)
    pub fn was_clicked(&mut self) -> bool {
        let clicked = self.clicked;
        self.clicked = false;
        clicked
    }

    /// Make this a primary action button
    pub fn primary(mut self) -> Self {
        self.background = theme::ACCENT;
        self.hover_background = Color::rgba(0.35, 0.70, 1.0, 1.0);  // Lighter blue on hover
        self.pressed_background = Color::rgba(0.20, 0.55, 0.85, 1.0); // Darker blue on press
        self.text_color = theme::TEXT_BRIGHT;
        self.border_color = None;
        self
    }

    /// Make this a ghost button (transparent bg, subtle border for visibility)
    pub fn ghost(mut self) -> Self {
        self.background = Color::rgba(0.0, 0.0, 0.0, 0.0);  // Fully transparent
        self.hover_background = Color::rgba(1.0, 1.0, 1.0, 0.08); // Subtle white overlay on hover
        self.pressed_background = Color::rgba(1.0, 1.0, 1.0, 0.12); // Slightly more visible on press
        self.text_color = theme::TEXT_MUTED;
        self.border_color = Some(theme::BORDER);  // Visible border so ghost buttons look like buttons
        self
    }

    fn current_background(&self) -> Color {
        if self.state.pressed {
            self.pressed_background
        } else if self.state.hovered {
            self.hover_background
        } else {
            self.background
        }
    }
}

impl Button {
    /// Layout with bold text for the label.
    pub fn layout_with_bold(&self, text_renderer: &TextRenderer, bold_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();
        let corner_radius = (bounds.height * 0.20).min(8.0);

        // Draw background with rounded corners
        let bg_color = self.current_background();
        output.spline_vertices.extend(create_rounded_rect_vertices(&bounds, bg_color.to_array(), corner_radius));

        // Draw border
        if let Some(border) = self.border_color {
            let border_color = if self.state.hovered {
                theme::BORDER_LIGHT
            } else {
                border
            };
            output.spline_vertices.extend(create_rounded_rect_outline_vertices(
                &bounds,
                border_color.to_array(),
                corner_radius,
                1.0,
            ));
        }

        // Top highlight line when hovered
        if self.state.hovered && self.border_color.is_some() {
            let highlight_rect = Rect::new(bounds.x + 1.0, bounds.y + 1.0, bounds.width - 2.0, 1.0);
            output.spline_vertices.extend(create_rect_vertices(
                &highlight_rect,
                theme::BORDER_LIGHT.with_alpha(0.7).to_array(),
            ));
        }

        // Draw label text in bold
        let line_height = text_renderer.line_height();

        let text_width = bold_renderer.measure_text(&self.label);
        let text_x = bounds.x + (bounds.width - text_width) / 2.0;
        let text_y = bounds.y + (bounds.height - line_height) / 2.0;

        let text_color = if self.state.hovered || self.state.pressed {
            theme::TEXT_BRIGHT
        } else {
            self.text_color
        };

        output.bold_text_vertices.extend(bold_renderer.layout_text(
            &self.label,
            text_x,
            text_y,
            text_color.to_array(),
        ));

        output
    }
}

impl Widget for Button {
    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        if !self.state.enabled {
            return EventResponse::Ignored;
        }

        match event {
            InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } => {
                if bounds.contains(*x, *y) {
                    self.state.pressed = true;
                    return EventResponse::Consumed;
                }
            }
            InputEvent::MouseUp { button: MouseButton::Left, x, y, .. } => {
                if self.state.pressed {
                    self.state.pressed = false;
                    if bounds.contains(*x, *y) {
                        self.clicked = true;
                        return EventResponse::Consumed;
                    }
                }
            }
            InputEvent::MouseMove { x, y, .. } => {
                let was_hovered = self.state.hovered;
                self.state.hovered = bounds.contains(*x, *y);
                if was_hovered != self.state.hovered {
                    // State changed, but don't consume the event
                }
            }
            _ => {}
        }

        EventResponse::Ignored
    }

    fn update_hover(&mut self, x: f32, y: f32, bounds: Rect) {
        self.state.hovered = bounds.contains(x, y);
    }

    fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();
        let corner_radius = (bounds.height * 0.20).min(8.0);

        // Press offset: shift content down 1px for tactile press feedback
        let press_offset = if self.state.pressed { 1.0 } else { 0.0 };
        let draw_bounds = Rect::new(bounds.x, bounds.y + press_offset, bounds.width, bounds.height);

        // Draw background with rounded corners
        let bg_color = self.current_background();
        output.spline_vertices.extend(create_rounded_rect_vertices(&draw_bounds, bg_color.to_array(), corner_radius));

        // Draw border - brighter on hover (using overlaid rounded rect for outline effect)
        if let Some(border) = self.border_color {
            let border_color = if self.state.pressed {
                border // Normal border when pressed (inset look)
            } else if self.state.hovered {
                theme::BORDER_LIGHT
            } else {
                border
            };
            output.spline_vertices.extend(create_rounded_rect_outline_vertices(
                &draw_bounds,
                border_color.to_array(),
                corner_radius,
                1.0,
            ));
        }

        // Top highlight line when hovered but NOT pressed (subtle depth effect)
        if self.state.hovered && !self.state.pressed && self.border_color.is_some() {
            let highlight_rect = Rect::new(draw_bounds.x + 1.0, draw_bounds.y + 1.0, draw_bounds.width - 2.0, 1.0);
            output.spline_vertices.extend(create_rect_vertices(
                &highlight_rect,
                theme::BORDER_LIGHT.with_alpha(0.7).to_array(),
            ));
        }

        // Draw label text - brighter on hover/press
        let line_height = text_renderer.line_height();

        let text_width = text_renderer.measure_text(&self.label);
        let text_x = draw_bounds.x + (draw_bounds.width - text_width) / 2.0;
        let text_y = draw_bounds.y + (draw_bounds.height - line_height) / 2.0;

        let text_color = if self.state.hovered || self.state.pressed {
            theme::TEXT_BRIGHT
        } else {
            self.text_color
        };

        output.text_vertices.extend(text_renderer.layout_text(
            &self.label,
            text_x,
            text_y,
            text_color.to_array(),
        ));

        output
    }

    fn set_focused(&mut self, focused: bool) {
        self.state.focused = focused;
    }
}
