//! Label widget - static text display

use crate::ui::widget::{LayoutCtx, Widget, WidgetOutput, theme};
use crate::ui::{Color, Rect};

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
    fn layout(&mut self, ctx: &LayoutCtx, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        if self.text.is_empty() {
            return output;
        }

        let line_height = ctx.text.line_height();
        let text_width = ctx.text.measure_text(&self.text);

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

        output.text_vertices.extend(ctx.text.layout_text(
            &self.text,
            x,
            y,
            self.color.to_array(),
        ));

        output
    }
}
