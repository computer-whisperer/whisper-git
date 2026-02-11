//! Layout module - rectangle math and screen layout

mod screen;

pub use screen::ScreenLayout;

/// A rectangle in screen coordinates (pixels, origin top-left)
#[derive(Clone, Copy, Debug, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self { x, y, width, height }
    }

    pub fn from_size(width: f32, height: f32) -> Self {
        Self { x: 0.0, y: 0.0, width, height }
    }

    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub fn inset(&self, amount: f32) -> Self {
        Self {
            x: self.x + amount,
            y: self.y + amount,
            width: (self.width - 2.0 * amount).max(0.0),
            height: (self.height - 2.0 * amount).max(0.0),
        }
    }

    pub fn pad(&self, left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            x: self.x + left,
            y: self.y + top,
            width: (self.width - left - right).max(0.0),
            height: (self.height - top - bottom).max(0.0),
        }
    }

    /// Split horizontally at a percentage from the left
    pub fn split_horizontal(&self, percent: f32) -> (Rect, Rect) {
        let split_x = self.width * percent.clamp(0.0, 1.0);
        let left = Rect::new(self.x, self.y, split_x, self.height);
        let right = Rect::new(self.x + split_x, self.y, self.width - split_x, self.height);
        (left, right)
    }

    /// Split vertically at a percentage from the top
    pub fn split_vertical(&self, percent: f32) -> (Rect, Rect) {
        let split_y = self.height * percent.clamp(0.0, 1.0);
        let top = Rect::new(self.x, self.y, self.width, split_y);
        let bottom = Rect::new(self.x, self.y + split_y, self.width, self.height - split_y);
        (top, bottom)
    }

    /// Take a fixed height from the top
    pub fn take_top(&self, height: f32) -> (Rect, Rect) {
        let height = height.min(self.height);
        let top = Rect::new(self.x, self.y, self.width, height);
        let bottom = Rect::new(self.x, self.y + height, self.width, self.height - height);
        (top, bottom)
    }

    /// Take a fixed width from the left
    pub fn take_left(&self, width: f32) -> (Rect, Rect) {
        let width = width.min(self.width);
        let left = Rect::new(self.x, self.y, width, self.height);
        let right = Rect::new(self.x + width, self.y, self.width - width, self.height);
        (left, right)
    }

    /// Take a fixed width from the right
    pub fn take_right(&self, width: f32) -> (Rect, Rect) {
        let width = width.min(self.width);
        let left = Rect::new(self.x, self.y, self.width - width, self.height);
        let right = Rect::new(self.x + self.width - width, self.y, width, self.height);
        (left, right)
    }
}

/// RGBA color
#[derive(Clone, Copy, Debug)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub fn to_array(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a]
    }

    /// Lighten the color
    pub fn lighten(&self, amount: f32) -> Self {
        Self {
            r: (self.r + amount).min(1.0),
            g: (self.g + amount).min(1.0),
            b: (self.b + amount).min(1.0),
            a: self.a,
        }
    }

    /// Set the alpha value
    pub fn with_alpha(&self, alpha: f32) -> Self {
        Self {
            r: self.r,
            g: self.g,
            b: self.b,
            a: alpha,
        }
    }

    pub const TRANSPARENT: Self = Self::rgba(0.0, 0.0, 0.0, 0.0);
}

impl Default for Color {
    fn default() -> Self {
        Self::rgb(1.0, 1.0, 1.0)
    }
}
