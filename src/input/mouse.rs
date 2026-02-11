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
}

impl MouseState {
    pub fn new() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            pressed: HashSet::new(),
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
        } else {
            self.pressed.remove(&button);
        }
    }

}

impl Default for MouseState {
    fn default() -> Self {
        Self::new()
    }
}
