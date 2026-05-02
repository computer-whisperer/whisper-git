//! Button widget - clickable with visual states

use crate::input::{EventResponse, InputEvent, MouseButton};
use crate::ui::text_util::truncate_to_width;
use crate::ui::widget::{
    LayoutCtx, Widget, WidgetOutput, WidgetState, create_rect_vertices,
    create_rounded_rect_outline_vertices, create_rounded_rect_vertices, theme,
};
use crate::ui::{Color, Rect};

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

    /// Make this a primary action button (orange)
    pub fn primary(mut self) -> Self {
        self.background = theme::ACTION;
        self.hover_background = Color::rgba(1.0, 0.65, 0.22, 1.0); // Lighter orange on hover
        self.pressed_background = Color::rgba(0.85, 0.48, 0.06, 1.0); // Darker orange on press
        self.text_color = Color::rgba(0.10, 0.08, 0.04, 1.0); // Dark text on orange
        self.border_color = None;
        self
    }

    /// Make this a ghost button (transparent bg, subtle border for visibility)
    pub fn ghost(mut self) -> Self {
        self.background = Color::rgba(0.0, 0.0, 0.0, 0.0); // Fully transparent
        self.hover_background = Color::rgba(1.0, 1.0, 1.0, 0.08); // Subtle white overlay on hover
        self.pressed_background = Color::rgba(1.0, 1.0, 1.0, 0.12); // Slightly more visible on press
        self.text_color = theme::TEXT_MUTED;
        self.border_color = Some(theme::BORDER); // Visible border so ghost buttons look like buttons
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
    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        if !self.state.enabled {
            return EventResponse::Ignored;
        }

        match event {
            InputEvent::MouseDown {
                button: MouseButton::Left,
                x,
                y,
                ..
            } if bounds.contains(*x, *y) => {
                self.state.pressed = true;
                return EventResponse::Consumed;
            }
            InputEvent::MouseUp {
                button: MouseButton::Left,
                x,
                y,
                ..
            } if self.state.pressed => {
                self.state.pressed = false;
                if bounds.contains(*x, *y) {
                    self.clicked = true;
                    return EventResponse::Consumed;
                }
            }
            InputEvent::MouseMove { x, y, .. } => {
                self.state.hovered = bounds.contains(*x, *y);
            }
            _ => {}
        }

        EventResponse::Ignored
    }

    fn update_hover(&mut self, x: f32, y: f32, bounds: Rect) {
        self.state.hovered = bounds.contains(x, y);
    }

    fn layout(&mut self, ctx: &LayoutCtx, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();
        let corner_radius = (bounds.height * 0.20).min(8.0);

        // Press offset: shift content down 1px for tactile press feedback
        let press_offset = if self.state.pressed { 1.0 } else { 0.0 };
        let draw_bounds = Rect::new(
            bounds.x,
            bounds.y + press_offset,
            bounds.width,
            bounds.height,
        );

        // Draw background with rounded corners
        output.spline_vertices.extend(create_rounded_rect_vertices(
            &draw_bounds,
            self.current_background().to_array(),
            corner_radius,
        ));

        // Draw border — brighter on hover, normal when pressed (inset look)
        if let Some(border) = self.border_color {
            let border_color = if self.state.pressed {
                border
            } else if self.state.hovered {
                theme::BORDER_LIGHT
            } else {
                border
            };
            output
                .spline_vertices
                .extend(create_rounded_rect_outline_vertices(
                    &draw_bounds,
                    border_color.to_array(),
                    corner_radius,
                    1.0,
                ));
        }

        // Top highlight line when hovered but NOT pressed (subtle depth effect)
        if self.state.hovered && !self.state.pressed && self.border_color.is_some() {
            let highlight_rect = Rect::new(
                draw_bounds.x + 1.0,
                draw_bounds.y + 1.0,
                draw_bounds.width - 2.0,
                1.0,
            );
            output.spline_vertices.extend(create_rect_vertices(
                &highlight_rect,
                theme::BORDER_LIGHT.with_alpha(0.7).to_array(),
            ));
        }

        // Bold label, truncated to the available width.
        let h_padding = 12.0;
        let max_text_width = bounds.width - h_padding * 2.0;
        let raw_w = ctx.bold.measure_text(&self.label);
        let (label, label_w) = if raw_w > max_text_width && max_text_width > 0.0 {
            let t = truncate_to_width(&self.label, ctx.bold, max_text_width);
            let w = ctx.bold.measure_text(&t);
            (t, w)
        } else {
            (self.label.clone(), raw_w)
        };

        let text_x = draw_bounds.x + (draw_bounds.width - label_w) / 2.0;
        let text_y = draw_bounds.y + (draw_bounds.height - ctx.bold.line_height()) / 2.0;
        let text_color = if self.state.hovered || self.state.pressed {
            theme::TEXT_BRIGHT
        } else {
            self.text_color
        };

        output.bold_text_vertices.extend(ctx.bold.layout_text(
            &label,
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
