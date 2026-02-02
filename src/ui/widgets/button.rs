//! Button widget - clickable with visual states

use crate::input::{InputEvent, EventResponse, MouseButton};
use crate::ui::{Color, Rect, TextRenderer};
use crate::ui::widget::{Widget, WidgetId, WidgetState, WidgetOutput, create_rect_vertices, create_rect_outline_vertices, theme};

/// A clickable button with text
pub struct Button {
    id: WidgetId,
    state: WidgetState,
    /// The button label
    pub label: String,
    /// Badge text (e.g., "+3" for commits ahead)
    pub badge: Option<String>,
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
    /// Padding inside the button
    pub padding: f32,
}

impl Button {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            id: WidgetId::new(),
            state: WidgetState::new(),
            label: label.into(),
            badge: None,
            clicked: false,
            background: theme::SURFACE_RAISED,
            hover_background: theme::SURFACE_HOVER,
            pressed_background: theme::SURFACE,
            text_color: theme::TEXT,
            border_color: Some(theme::BORDER),
            padding: 8.0,
        }
    }

    /// Set a badge (e.g., count indicator)
    pub fn with_badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    /// Check if the button was clicked this frame (and clear the flag)
    pub fn was_clicked(&mut self) -> bool {
        let clicked = self.clicked;
        self.clicked = false;
        clicked
    }

    /// Set the background color scheme
    pub fn with_colors(mut self, normal: Color, hover: Color, pressed: Color) -> Self {
        self.background = normal;
        self.hover_background = hover;
        self.pressed_background = pressed;
        self
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

impl Widget for Button {
    fn id(&self) -> WidgetId {
        self.id
    }

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

        // Draw background
        let bg_color = self.current_background();
        output.spline_vertices.extend(create_rect_vertices(&bounds, bg_color.to_array()));

        // Draw border - brighter on hover
        if let Some(border) = self.border_color {
            let border_color = if self.state.hovered {
                theme::BORDER_LIGHT
            } else {
                border
            };
            output.spline_vertices.extend(create_rect_outline_vertices(
                &bounds,
                border_color.to_array(),
                1.0,
            ));
        }

        // Top highlight line when hovered (subtle depth effect)
        if self.state.hovered && self.border_color.is_some() {
            let highlight_rect = Rect::new(bounds.x + 1.0, bounds.y + 1.0, bounds.width - 2.0, 1.0);
            output.spline_vertices.extend(create_rect_vertices(
                &highlight_rect,
                theme::BORDER_LIGHT.with_alpha(0.5).to_array(),
            ));
        }

        // Draw label text - brighter on hover
        let line_height = text_renderer.line_height();
        let display_text = if let Some(ref badge) = self.badge {
            format!("{} ({})", self.label, badge)
        } else {
            self.label.clone()
        };

        let text_width = text_renderer.measure_text(&display_text);
        let text_x = bounds.x + (bounds.width - text_width) / 2.0;
        let text_y = bounds.y + (bounds.height - line_height) / 2.0;

        let text_color = if self.state.hovered || self.state.pressed {
            theme::TEXT_BRIGHT
        } else {
            self.text_color
        };

        output.text_vertices.extend(text_renderer.layout_text(
            &display_text,
            text_x,
            text_y,
            text_color.to_array(),
        ));

        output
    }

    fn focusable(&self) -> bool {
        self.state.enabled
    }

    fn set_focused(&mut self, focused: bool) {
        self.state.focused = focused;
    }

    fn preferred_size(&self, text_renderer: &TextRenderer) -> (f32, f32) {
        let line_height = text_renderer.line_height();
        let text = if let Some(ref badge) = self.badge {
            format!("{} ({})", self.label, badge)
        } else {
            self.label.clone()
        };
        let text_width = text_renderer.measure_text(&text);
        (text_width + self.padding * 2.0, line_height + self.padding * 2.0)
    }
}
