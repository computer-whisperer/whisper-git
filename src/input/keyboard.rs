//! Keyboard input handling

use std::collections::HashSet;
use winit::keyboard::{KeyCode, PhysicalKey};

/// Keyboard modifier keys
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub super_key: bool,
}

impl Modifiers {
    pub const fn empty() -> Self {
        Self {
            shift: false,
            ctrl: false,
            alt: false,
            super_key: false,
        }
    }

    pub fn from_winit(mods: winit::keyboard::ModifiersState) -> Self {
        Self {
            shift: mods.shift_key(),
            ctrl: mods.control_key(),
            alt: mods.alt_key(),
            super_key: mods.super_key(),
        }
    }

    /// Check if any modifier is pressed
    pub fn any(&self) -> bool {
        self.shift || self.ctrl || self.alt || self.super_key
    }

    /// Check if only shift is pressed (for Shift+Tab, etc.)
    pub fn only_shift(&self) -> bool {
        self.shift && !self.ctrl && !self.alt && !self.super_key
    }

    /// Check if only ctrl is pressed (for Ctrl+C, etc.)
    pub fn only_ctrl(&self) -> bool {
        self.ctrl && !self.shift && !self.alt && !self.super_key
    }

    /// Check if Ctrl+Shift is pressed
    pub fn ctrl_shift(&self) -> bool {
        self.ctrl && self.shift && !self.alt && !self.super_key
    }
}

/// A logical key representation
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    // Letters
    A, B, C, D, E, F, G, H, I, J, K, L, M,
    N, O, P, Q, R, S, T, U, V, W, X, Y, Z,

    // Numbers
    Num0, Num1, Num2, Num3, Num4,
    Num5, Num6, Num7, Num8, Num9,

    // Function keys
    F1, F2, F3, F4, F5, F6,
    F7, F8, F9, F10, F11, F12,

    // Navigation
    Up, Down, Left, Right,
    Home, End, PageUp, PageDown,

    // Editing
    Backspace, Delete, Insert,
    Enter, Tab,

    // Modifiers (for display purposes)
    Shift, Ctrl, Alt, Super,

    // Special
    Escape,
    Space,

    // Punctuation
    Minus,
    Equals,
    LeftBracket,
    RightBracket,
    Backslash,
    Semicolon,
    Quote,
    Comma,
    Period,
    Slash,
    Grave,

    // Unknown/other
    Unknown,
}

impl Key {
    /// Convert from winit key codes
    pub fn from_winit(
        physical: &PhysicalKey,
        _logical: &winit::keyboard::Key,
    ) -> Self {
        match physical {
            PhysicalKey::Code(code) => match code {
                // Letters
                KeyCode::KeyA => Key::A,
                KeyCode::KeyB => Key::B,
                KeyCode::KeyC => Key::C,
                KeyCode::KeyD => Key::D,
                KeyCode::KeyE => Key::E,
                KeyCode::KeyF => Key::F,
                KeyCode::KeyG => Key::G,
                KeyCode::KeyH => Key::H,
                KeyCode::KeyI => Key::I,
                KeyCode::KeyJ => Key::J,
                KeyCode::KeyK => Key::K,
                KeyCode::KeyL => Key::L,
                KeyCode::KeyM => Key::M,
                KeyCode::KeyN => Key::N,
                KeyCode::KeyO => Key::O,
                KeyCode::KeyP => Key::P,
                KeyCode::KeyQ => Key::Q,
                KeyCode::KeyR => Key::R,
                KeyCode::KeyS => Key::S,
                KeyCode::KeyT => Key::T,
                KeyCode::KeyU => Key::U,
                KeyCode::KeyV => Key::V,
                KeyCode::KeyW => Key::W,
                KeyCode::KeyX => Key::X,
                KeyCode::KeyY => Key::Y,
                KeyCode::KeyZ => Key::Z,

                // Numbers
                KeyCode::Digit0 => Key::Num0,
                KeyCode::Digit1 => Key::Num1,
                KeyCode::Digit2 => Key::Num2,
                KeyCode::Digit3 => Key::Num3,
                KeyCode::Digit4 => Key::Num4,
                KeyCode::Digit5 => Key::Num5,
                KeyCode::Digit6 => Key::Num6,
                KeyCode::Digit7 => Key::Num7,
                KeyCode::Digit8 => Key::Num8,
                KeyCode::Digit9 => Key::Num9,

                // Function keys
                KeyCode::F1 => Key::F1,
                KeyCode::F2 => Key::F2,
                KeyCode::F3 => Key::F3,
                KeyCode::F4 => Key::F4,
                KeyCode::F5 => Key::F5,
                KeyCode::F6 => Key::F6,
                KeyCode::F7 => Key::F7,
                KeyCode::F8 => Key::F8,
                KeyCode::F9 => Key::F9,
                KeyCode::F10 => Key::F10,
                KeyCode::F11 => Key::F11,
                KeyCode::F12 => Key::F12,

                // Navigation
                KeyCode::ArrowUp => Key::Up,
                KeyCode::ArrowDown => Key::Down,
                KeyCode::ArrowLeft => Key::Left,
                KeyCode::ArrowRight => Key::Right,
                KeyCode::Home => Key::Home,
                KeyCode::End => Key::End,
                KeyCode::PageUp => Key::PageUp,
                KeyCode::PageDown => Key::PageDown,

                // Editing
                KeyCode::Backspace => Key::Backspace,
                KeyCode::Delete => Key::Delete,
                KeyCode::Insert => Key::Insert,
                KeyCode::Enter | KeyCode::NumpadEnter => Key::Enter,
                KeyCode::Tab => Key::Tab,

                // Special
                KeyCode::Escape => Key::Escape,
                KeyCode::Space => Key::Space,

                // Punctuation
                KeyCode::Minus => Key::Minus,
                KeyCode::Equal => Key::Equals,
                KeyCode::BracketLeft => Key::LeftBracket,
                KeyCode::BracketRight => Key::RightBracket,
                KeyCode::Backslash => Key::Backslash,
                KeyCode::Semicolon => Key::Semicolon,
                KeyCode::Quote => Key::Quote,
                KeyCode::Comma => Key::Comma,
                KeyCode::Period => Key::Period,
                KeyCode::Slash => Key::Slash,
                KeyCode::Backquote => Key::Grave,

                // Modifiers
                KeyCode::ShiftLeft | KeyCode::ShiftRight => Key::Shift,
                KeyCode::ControlLeft | KeyCode::ControlRight => Key::Ctrl,
                KeyCode::AltLeft | KeyCode::AltRight => Key::Alt,
                KeyCode::SuperLeft | KeyCode::SuperRight => Key::Super,

                _ => Key::Unknown,
            },
            PhysicalKey::Unidentified(_) => Key::Unknown,
        }
    }

    /// Check if this is a printable character key
    pub fn is_printable(&self) -> bool {
        matches!(
            self,
            Key::A | Key::B | Key::C | Key::D | Key::E | Key::F | Key::G | Key::H |
            Key::I | Key::J | Key::K | Key::L | Key::M | Key::N | Key::O | Key::P |
            Key::Q | Key::R | Key::S | Key::T | Key::U | Key::V | Key::W | Key::X |
            Key::Y | Key::Z | Key::Num0 | Key::Num1 | Key::Num2 | Key::Num3 |
            Key::Num4 | Key::Num5 | Key::Num6 | Key::Num7 | Key::Num8 | Key::Num9 |
            Key::Space | Key::Minus | Key::Equals | Key::LeftBracket | Key::RightBracket |
            Key::Backslash | Key::Semicolon | Key::Quote | Key::Comma | Key::Period |
            Key::Slash | Key::Grave
        )
    }

    /// Check if this is a navigation key
    pub fn is_navigation(&self) -> bool {
        matches!(
            self,
            Key::Up | Key::Down | Key::Left | Key::Right |
            Key::Home | Key::End | Key::PageUp | Key::PageDown
        )
    }

    /// Check if this is a modifier key
    pub fn is_modifier(&self) -> bool {
        matches!(self, Key::Shift | Key::Ctrl | Key::Alt | Key::Super)
    }
}

/// Tracks which keys are currently pressed
pub struct KeyState {
    pressed: HashSet<Key>,
}

impl KeyState {
    pub fn new() -> Self {
        Self {
            pressed: HashSet::new(),
        }
    }

    pub fn set_pressed(&mut self, key: Key, pressed: bool) {
        if pressed {
            self.pressed.insert(key);
        } else {
            self.pressed.remove(&key);
        }
    }

    pub fn is_pressed(&self, key: Key) -> bool {
        self.pressed.contains(&key)
    }

    pub fn clear(&mut self) {
        self.pressed.clear();
    }
}

impl Default for KeyState {
    fn default() -> Self {
        Self::new()
    }
}
