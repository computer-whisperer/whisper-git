//! Label widget - static text display

use crate::ui::{Color, Rect, TextRenderer};
use crate::ui::widget::{Widget, WidgetId, WidgetOutput, theme};

/// Text alignment options
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// A static text label
pub struct Label {
    id: WidgetId,
    /// The text to display
    pub text: String,
    /// Text color
    pub color: Color,
    /// Horizontal alignment
    pub align: TextAlign,
    /// Vertical centering
    pub vertical_center: bool,
}

impl Label {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            id: WidgetId::new(),
            text: text.into(),
            color: theme::TEXT,
            align: TextAlign::Left,
            vertical_center: true,
        }
    }

    /// Set the text color
    pub fn with_color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    /// Set muted text color
    pub fn muted(mut self) -> Self {
        self.color = theme::TEXT_MUTED;
        self
    }

    /// Set text alignment
    pub fn with_align(mut self, align: TextAlign) -> Self {
        self.align = align;
        self
    }

    /// Center the text horizontally
    pub fn centered(mut self) -> Self {
        self.align = TextAlign::Center;
        self
    }

    /// Update the text content
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }
}

impl Widget for Label {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        if self.text.is_empty() {
            return output;
        }

        let line_height = text_renderer.line_height();

        // Calculate text width for alignment
        // Rough estimate: ~10px per character (monospace font)
        let text_width = self.text.len() as f32 * 10.0;

        let x = match self.align {
            TextAlign::Left => bounds.x,
            TextAlign::Center => bounds.x + (bounds.width - text_width) / 2.0,
            TextAlign::Right => bounds.right() - text_width,
        };

        let y = if self.vertical_center {
            bounds.y + (bounds.height - line_height) / 2.0
        } else {
            bounds.y
        };

        output.text_vertices.extend(text_renderer.layout_text(
            &self.text,
            x,
            y,
            self.color.to_array(),
        ));

        output
    }

    fn preferred_size(&self, text_renderer: &TextRenderer) -> (f32, f32) {
        let line_height = text_renderer.line_height();
        let text_width = self.text.len() as f32 * 10.0; // Rough estimate
        (text_width, line_height)
    }
}
