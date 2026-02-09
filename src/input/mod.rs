//! Input handling system for whisper-git
//!
//! Provides a unified input event system that abstracts winit events into
//! application-level input events with keyboard/mouse state tracking.

mod keyboard;
mod mouse;

pub use keyboard::{Key, KeyState, Modifiers};
pub use mouse::{MouseButton, MouseState};

use crate::ui::Rect;

/// A unified input event for the application
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum InputEvent {
    /// A key was pressed or released
    KeyDown {
        key: Key,
        modifiers: Modifiers,
        /// Character text from winit's logical key (for text insertion fallback
        /// when IME doesn't fire on X11/Wayland)
        text: Option<String>,
    },
    KeyUp {
        key: Key,
        modifiers: Modifiers,
    },

    /// Text input (for text fields)
    TextInput(String),

    /// Mouse button pressed
    MouseDown {
        button: MouseButton,
        x: f32,
        y: f32,
        modifiers: Modifiers,
    },

    /// Mouse button released
    MouseUp {
        button: MouseButton,
        x: f32,
        y: f32,
        modifiers: Modifiers,
    },

    /// Mouse moved
    MouseMove {
        x: f32,
        y: f32,
        modifiers: Modifiers,
    },

    /// Mouse scroll (wheel)
    Scroll {
        delta_x: f32,
        delta_y: f32,
        x: f32,
        y: f32,
        modifiers: Modifiers,
    },
}

impl InputEvent {
    /// Get the position of a mouse event, if applicable
    pub fn position(&self) -> Option<(f32, f32)> {
        match self {
            InputEvent::MouseDown { x, y, .. }
            | InputEvent::MouseUp { x, y, .. }
            | InputEvent::MouseMove { x, y, .. }
            | InputEvent::Scroll { x, y, .. } => Some((*x, *y)),
            _ => None,
        }
    }

    /// Check if this event is within a given rect
    #[allow(dead_code)]
    pub fn is_within(&self, rect: &Rect) -> bool {
        if let Some((x, y)) = self.position() {
            rect.contains(x, y)
        } else {
            false
        }
    }

    /// Get modifiers for this event
    #[allow(dead_code)]
    pub fn modifiers(&self) -> Modifiers {
        match self {
            InputEvent::KeyDown { modifiers, .. }
            | InputEvent::KeyUp { modifiers, .. }
            | InputEvent::MouseDown { modifiers, .. }
            | InputEvent::MouseUp { modifiers, .. }
            | InputEvent::MouseMove { modifiers, .. }
            | InputEvent::Scroll { modifiers, .. } => *modifiers,
            InputEvent::TextInput(_) => Modifiers::empty(),
        }
    }
}

/// Response from handling an input event
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EventResponse {
    /// Event was not handled, should bubble up
    #[default]
    Ignored,
    /// Event was handled, stop propagation
    Consumed,
}

impl EventResponse {
    pub fn is_consumed(&self) -> bool {
        matches!(self, EventResponse::Consumed)
    }
}

/// Tracks overall input state
pub struct InputState {
    pub keyboard: KeyState,
    pub mouse: MouseState,
    pub modifiers: Modifiers,
}

impl InputState {
    pub fn new() -> Self {
        Self {
            keyboard: KeyState::new(),
            mouse: MouseState::new(),
            modifiers: Modifiers::empty(),
        }
    }

    /// Update state from a winit WindowEvent and optionally produce an InputEvent
    pub fn handle_window_event(&mut self, event: &winit::event::WindowEvent) -> Option<InputEvent> {
        use winit::event::WindowEvent;

        match event {
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = Modifiers::from_winit(mods.state());
                None
            }

            WindowEvent::KeyboardInput { event, .. } => {
                let key = Key::from_winit(&event.physical_key, &event.logical_key);

                // Extract character text from winit's logical key for text insertion fallback.
                // This handles keyboard layouts correctly without a manual mapping table.
                let text = if let winit::keyboard::Key::Character(s) = &event.logical_key {
                    Some(s.to_string())
                } else {
                    None
                };

                match event.state {
                    winit::event::ElementState::Pressed => {
                        self.keyboard.set_pressed(key, true);
                        Some(InputEvent::KeyDown {
                            key,
                            modifiers: self.modifiers,
                            text,
                        })
                    }
                    winit::event::ElementState::Released => {
                        self.keyboard.set_pressed(key, false);
                        Some(InputEvent::KeyUp {
                            key,
                            modifiers: self.modifiers,
                        })
                    }
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                let x = position.x as f32;
                let y = position.y as f32;
                self.mouse.update_position(x, y);
                Some(InputEvent::MouseMove {
                    x,
                    y,
                    modifiers: self.modifiers,
                })
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let button = MouseButton::from_winit(*button);
                let (x, y) = self.mouse.position();

                match state {
                    winit::event::ElementState::Pressed => {
                        self.mouse.set_pressed(button, true);
                        Some(InputEvent::MouseDown {
                            button,
                            x,
                            y,
                            modifiers: self.modifiers,
                        })
                    }
                    winit::event::ElementState::Released => {
                        self.mouse.set_pressed(button, false);
                        Some(InputEvent::MouseUp {
                            button,
                            x,
                            y,
                            modifiers: self.modifiers,
                        })
                    }
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let (delta_x, delta_y) = match delta {
                    winit::event::MouseScrollDelta::LineDelta(x, y) => (*x * 20.0, *y * 20.0),
                    winit::event::MouseScrollDelta::PixelDelta(pos) => {
                        (pos.x as f32, pos.y as f32)
                    }
                };
                let (x, y) = self.mouse.position();
                Some(InputEvent::Scroll {
                    delta_x,
                    delta_y,
                    x,
                    y,
                    modifiers: self.modifiers,
                })
            }

            WindowEvent::Ime(ime) => {
                use winit::event::Ime;
                match ime {
                    Ime::Commit(text) => {
                        Some(InputEvent::TextInput(text.clone()))
                    }
                    _ => None,
                }
            }

            _ => None,
        }
    }
}

impl Default for InputState {
    fn default() -> Self {
        Self::new()
    }
}
