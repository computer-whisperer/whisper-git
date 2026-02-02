//! Panel widget - a container with optional background and border

use crate::ui::{Color, Rect, TextRenderer};
use crate::ui::widget::{Widget, WidgetId, WidgetOutput, create_rect_vertices, create_rect_outline_vertices, theme};

/// A panel container with background color and optional border
pub struct Panel {
    id: WidgetId,
    /// Background color (None for transparent)
    pub background: Option<Color>,
    /// Border color (None for no border)
    pub border: Option<Color>,
    /// Border thickness
    pub border_thickness: f32,
    /// Padding inside the panel
    pub padding: f32,
}

impl Panel {
    pub fn new() -> Self {
        Self {
            id: WidgetId::new(),
            background: Some(theme::SURFACE),
            border: Some(theme::BORDER),
            border_thickness: 1.0,
            padding: 8.0,
        }
    }

    /// Create a panel with a specific background color
    pub fn with_background(mut self, color: Color) -> Self {
        self.background = Some(color);
        self
    }

    /// Create a transparent panel
    pub fn transparent(mut self) -> Self {
        self.background = None;
        self
    }

    /// Set the border color
    pub fn with_border(mut self, color: Color) -> Self {
        self.border = Some(color);
        self
    }

    /// Remove the border
    pub fn no_border(mut self) -> Self {
        self.border = None;
        self
    }

    /// Set padding
    pub fn with_padding(mut self, padding: f32) -> Self {
        self.padding = padding;
        self
    }

    /// Get the content bounds (inside padding)
    pub fn content_bounds(&self, bounds: Rect) -> Rect {
        bounds.inset(self.padding + self.border_thickness)
    }
}

impl Default for Panel {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for Panel {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn layout(&self, _text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        // Draw background
        if let Some(bg) = self.background {
            output.spline_vertices.extend(create_rect_vertices(&bounds, bg.to_array()));
        }

        // Draw border
        if let Some(border) = self.border {
            output.spline_vertices.extend(create_rect_outline_vertices(
                &bounds,
                border.to_array(),
                self.border_thickness,
            ));
        }

        output
    }
}
