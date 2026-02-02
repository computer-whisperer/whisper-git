//! Flex layout system for arranging widgets

use super::Rect;

/// Direction for flex layout
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlexDirection {
    /// Arrange items horizontally (row)
    #[default]
    Row,
    /// Arrange items vertically (column)
    Column,
}

/// How to size a flex item
#[derive(Clone, Copy, Debug)]
pub enum FlexSize {
    /// Take a fixed amount of space (pixels)
    Fixed(f32),
    /// Take a percentage of available space
    Percent(f32),
    /// Grow to fill available space (flex-grow factor)
    Flex(f32),
}

impl Default for FlexSize {
    fn default() -> Self {
        FlexSize::Flex(1.0)
    }
}

/// A single item in a flex layout
#[derive(Clone, Debug)]
pub struct FlexItem {
    /// How this item is sized
    pub size: FlexSize,
    /// Optional name for debugging
    pub name: Option<String>,
}

impl FlexItem {
    pub fn fixed(size: f32) -> Self {
        Self {
            size: FlexSize::Fixed(size),
            name: None,
        }
    }

    pub fn percent(percent: f32) -> Self {
        Self {
            size: FlexSize::Percent(percent),
            name: None,
        }
    }

    pub fn flex(factor: f32) -> Self {
        Self {
            size: FlexSize::Flex(factor),
            name: None,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

impl Default for FlexItem {
    fn default() -> Self {
        Self {
            size: FlexSize::Flex(1.0),
            name: None,
        }
    }
}

/// A flex container that arranges items in a row or column
#[derive(Clone, Debug, Default)]
pub struct FlexLayout {
    /// The direction to arrange items
    pub direction: FlexDirection,
    /// The items in this layout
    pub items: Vec<FlexItem>,
    /// Gap between items
    pub gap: f32,
}

impl FlexLayout {
    pub fn new(direction: FlexDirection) -> Self {
        Self {
            direction,
            items: Vec::new(),
            gap: 0.0,
        }
    }

    pub fn row() -> Self {
        Self::new(FlexDirection::Row)
    }

    pub fn column() -> Self {
        Self::new(FlexDirection::Column)
    }

    /// Add a fixed-size item
    pub fn add_fixed(&mut self, size: f32) -> &mut Self {
        self.items.push(FlexItem::fixed(size));
        self
    }

    /// Add a percentage-based item
    pub fn add_percent(&mut self, percent: f32) -> &mut Self {
        self.items.push(FlexItem::percent(percent));
        self
    }

    /// Add a flex-grow item
    pub fn add_flex(&mut self, factor: f32) -> &mut Self {
        self.items.push(FlexItem::flex(factor));
        self
    }

    /// Add a flex-grow item with factor 1
    pub fn add_fill(&mut self) -> &mut Self {
        self.items.push(FlexItem::flex(1.0));
        self
    }

    /// Add any flex item
    pub fn add(&mut self, item: FlexItem) -> &mut Self {
        self.items.push(item);
        self
    }

    /// Set the gap between items
    pub fn with_gap(&mut self, gap: f32) -> &mut Self {
        self.gap = gap;
        self
    }

    /// Compute the bounds for each item given the container bounds
    pub fn compute(&self, bounds: Rect) -> Vec<Rect> {
        if self.items.is_empty() {
            return Vec::new();
        }

        let total_gap = self.gap * (self.items.len() - 1) as f32;
        let available = match self.direction {
            FlexDirection::Row => bounds.width - total_gap,
            FlexDirection::Column => bounds.height - total_gap,
        };

        // First pass: calculate fixed and percentage sizes
        let mut remaining = available;
        let mut total_flex = 0.0;

        for item in &self.items {
            match item.size {
                FlexSize::Fixed(size) => {
                    remaining -= size;
                }
                FlexSize::Percent(percent) => {
                    remaining -= available * percent;
                }
                FlexSize::Flex(factor) => {
                    total_flex += factor;
                }
            }
        }

        // Handle case where remaining space is negative
        remaining = remaining.max(0.0);

        // Second pass: compute actual bounds
        let mut rects = Vec::with_capacity(self.items.len());
        let mut offset = match self.direction {
            FlexDirection::Row => bounds.x,
            FlexDirection::Column => bounds.y,
        };

        for item in &self.items {
            let size = match item.size {
                FlexSize::Fixed(size) => size,
                FlexSize::Percent(percent) => available * percent,
                FlexSize::Flex(factor) => {
                    if total_flex > 0.0 {
                        remaining * (factor / total_flex)
                    } else {
                        0.0
                    }
                }
            };

            let rect = match self.direction {
                FlexDirection::Row => Rect::new(offset, bounds.y, size, bounds.height),
                FlexDirection::Column => Rect::new(bounds.x, offset, bounds.width, size),
            };

            rects.push(rect);
            offset += size + self.gap;
        }

        rects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flex_row_percent() {
        let mut layout = FlexLayout::row();
        layout.add_percent(0.55);
        layout.add_percent(0.45);

        let bounds = Rect::from_size(1000.0, 100.0);
        let rects = layout.compute(bounds);

        assert_eq!(rects.len(), 2);
        assert!((rects[0].width - 550.0).abs() < 0.01);
        assert!((rects[1].width - 450.0).abs() < 0.01);
    }

    #[test]
    fn test_flex_column_fixed_and_flex() {
        let mut layout = FlexLayout::column();
        layout.add_fixed(40.0);  // Header
        layout.add_flex(1.0);   // Main content

        let bounds = Rect::from_size(100.0, 1000.0);
        let rects = layout.compute(bounds);

        assert_eq!(rects.len(), 2);
        assert!((rects[0].height - 40.0).abs() < 0.01);
        assert!((rects[1].height - 960.0).abs() < 0.01);
    }
}
