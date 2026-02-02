//! Mouse input handling

use std::collections::HashSet;

/// Mouse buttons
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
    Other(u16),
}

impl MouseButton {
    pub fn from_winit(button: winit::event::MouseButton) -> Self {
        match button {
            winit::event::MouseButton::Left => MouseButton::Left,
            winit::event::MouseButton::Right => MouseButton::Right,
            winit::event::MouseButton::Middle => MouseButton::Middle,
            winit::event::MouseButton::Back => MouseButton::Back,
            winit::event::MouseButton::Forward => MouseButton::Forward,
            winit::event::MouseButton::Other(id) => MouseButton::Other(id),
        }
    }
}

/// Tracks mouse state (position and button states)
pub struct MouseState {
    x: f32,
    y: f32,
    pressed: HashSet<MouseButton>,
    /// Position when drag started (if dragging)
    drag_start: Option<(f32, f32)>,
    /// Button that initiated the drag
    drag_button: Option<MouseButton>,
}

impl MouseState {
    pub fn new() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            pressed: HashSet::new(),
            drag_start: None,
            drag_button: None,
        }
    }

    pub fn position(&self) -> (f32, f32) {
        (self.x, self.y)
    }

    pub fn update_position(&mut self, x: f32, y: f32) {
        self.x = x;
        self.y = y;
    }

    pub fn set_pressed(&mut self, button: MouseButton, pressed: bool) {
        if pressed {
            self.pressed.insert(button);
            // Start tracking potential drag
            if self.drag_start.is_none() {
                self.drag_start = Some((self.x, self.y));
                self.drag_button = Some(button);
            }
        } else {
            self.pressed.remove(&button);
            // End drag if this was the drag button
            if self.drag_button == Some(button) {
                self.drag_start = None;
                self.drag_button = None;
            }
        }
    }

    pub fn is_pressed(&self, button: MouseButton) -> bool {
        self.pressed.contains(&button)
    }

    /// Check if currently dragging (button held and moved beyond threshold)
    pub fn is_dragging(&self) -> bool {
        if let Some((start_x, start_y)) = self.drag_start {
            let dx = self.x - start_x;
            let dy = self.y - start_y;
            let distance = (dx * dx + dy * dy).sqrt();
            distance > 5.0 // 5 pixel threshold
        } else {
            false
        }
    }

    /// Get drag delta if dragging
    pub fn drag_delta(&self) -> Option<(f32, f32)> {
        self.drag_start.map(|(start_x, start_y)| {
            (self.x - start_x, self.y - start_y)
        })
    }

    /// Get the button being used for dragging
    pub fn drag_button(&self) -> Option<MouseButton> {
        self.drag_button
    }

    pub fn clear(&mut self) {
        self.pressed.clear();
        self.drag_start = None;
        self.drag_button = None;
    }
}

impl Default for MouseState {
    fn default() -> Self {
        Self::new()
    }
}
