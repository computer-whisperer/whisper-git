//! Context menu widget - right-click popup overlay

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{create_rect_vertices, create_rect_outline_vertices, theme, WidgetOutput};
use crate::ui::{Rect, TextRenderer};

/// A single item in the context menu
#[derive(Clone, Debug)]
pub struct MenuItem {
    pub label: String,
    pub shortcut: Option<String>,
    pub action_id: String,
}

impl MenuItem {
    pub fn new(label: impl Into<String>, action_id: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            shortcut: None,
            action_id: action_id.into(),
        }
    }

    pub fn with_shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }
}

/// Result from a menu interaction
#[derive(Clone, Debug)]
pub enum MenuAction {
    Selected(String),
}

/// A popup context menu overlay
pub struct ContextMenu {
    visible: bool,
    items: Vec<MenuItem>,
    /// Position where the menu was opened (top-left of menu)
    pos_x: f32,
    pos_y: f32,
    /// Which item is hovered
    hovered_index: Option<usize>,
    /// Pending action to be consumed
    pending_action: Option<MenuAction>,
    /// Menu dimensions (computed during layout)
    menu_width: f32,
    item_height: f32,
}

impl ContextMenu {
    pub fn new() -> Self {
        Self {
            visible: false,
            items: Vec::new(),
            pos_x: 0.0,
            pos_y: 0.0,
            hovered_index: None,
            pending_action: None,
            menu_width: 200.0,
            item_height: 24.0,
        }
    }

    /// Show the context menu at a given position with the specified items
    pub fn show(&mut self, items: Vec<MenuItem>, x: f32, y: f32) {
        self.items = items;
        self.pos_x = x;
        self.pos_y = y;
        self.visible = true;
        self.hovered_index = None;
        self.pending_action = None;
    }

    /// Hide the context menu
    pub fn hide(&mut self) {
        self.visible = false;
        self.items.clear();
        self.hovered_index = None;
    }

    /// Check if the context menu is currently visible
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Take the pending action (consume it)
    pub fn take_action(&mut self) -> Option<MenuAction> {
        self.pending_action.take()
    }

    /// Get the bounding rectangle of the menu
    fn menu_bounds(&self) -> Rect {
        let height = self.items.len() as f32 * self.item_height + 4.0; // 2px padding top+bottom
        Rect::new(self.pos_x, self.pos_y, self.menu_width, height)
    }

    /// Clamp menu position so it stays within screen bounds
    fn clamp_position(&mut self, screen: Rect) {
        let height = self.items.len() as f32 * self.item_height + 4.0;

        if self.pos_x + self.menu_width > screen.right() {
            self.pos_x = (screen.right() - self.menu_width).max(screen.x);
        }
        if self.pos_y + height > screen.bottom() {
            self.pos_y = (screen.bottom() - height).max(screen.y);
        }
    }

    /// Handle an input event. Returns EventResponse::Consumed if the menu handled it.
    pub fn handle_event(&mut self, event: &InputEvent, _screen_bounds: Rect) -> EventResponse {
        if !self.visible {
            return EventResponse::Ignored;
        }

        let bounds = self.menu_bounds();

        match event {
            InputEvent::MouseMove { x, y, .. } => {
                if bounds.contains(*x, *y) {
                    let rel_y = *y - bounds.y - 2.0; // account for padding
                    let idx = (rel_y / self.item_height) as usize;
                    if idx < self.items.len() {
                        self.hovered_index = Some(idx);
                    } else {
                        self.hovered_index = None;
                    }
                } else {
                    self.hovered_index = None;
                }
                EventResponse::Consumed
            }
            InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } => {
                if bounds.contains(*x, *y) {
                    let rel_y = *y - bounds.y - 2.0;
                    let idx = (rel_y / self.item_height) as usize;
                    if idx < self.items.len() {
                        let action_id = self.items[idx].action_id.clone();
                        self.pending_action = Some(MenuAction::Selected(action_id));
                        self.hide();
                    }
                } else {
                    // Click outside menu -> close
                    self.hide();
                }
                EventResponse::Consumed
            }
            InputEvent::MouseDown { button: MouseButton::Right, .. } => {
                // Another right-click while menu is open -> close
                self.hide();
                EventResponse::Consumed
            }
            InputEvent::KeyDown { key: Key::Escape, .. } => {
                self.hide();
                EventResponse::Consumed
            }
            // Consume all other events while visible (prevent interaction with widgets behind)
            InputEvent::MouseUp { .. } | InputEvent::Scroll { .. } => {
                EventResponse::Consumed
            }
            _ => EventResponse::Ignored,
        }
    }

    /// Layout the context menu and produce rendering output.
    /// Call this LAST in the draw order so it renders on top.
    pub fn layout(&mut self, text_renderer: &TextRenderer, screen_bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        if !self.visible || self.items.is_empty() {
            return output;
        }

        // Update item height based on font metrics
        self.item_height = (text_renderer.line_height() * 1.6).max(22.0);
        self.menu_width = 200.0;

        // Compute menu width based on content
        for item in &self.items {
            let label_width = text_renderer.measure_text(&item.label);
            let shortcut_width = item.shortcut.as_ref()
                .map(|s| text_renderer.measure_text(s) + 24.0)
                .unwrap_or(0.0);
            let total = label_width + shortcut_width + 32.0; // padding
            if total > self.menu_width {
                self.menu_width = total;
            }
        }

        // Clamp position to screen
        self.clamp_position(screen_bounds);

        let bounds = self.menu_bounds();
        let line_height = text_renderer.line_height();

        // Shadow (offset dark rect behind menu)
        let shadow_rect = Rect::new(bounds.x + 3.0, bounds.y + 3.0, bounds.width, bounds.height);
        output.spline_vertices.extend(create_rect_vertices(
            &shadow_rect,
            [0.0, 0.0, 0.0, 0.4],
        ));

        // Menu background
        let bg_color = theme::SURFACE_RAISED.lighten(0.02);
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            bg_color.to_array(),
        ));

        // Menu border
        output.spline_vertices.extend(create_rect_outline_vertices(
            &bounds,
            theme::BORDER_LIGHT.to_array(),
            1.0,
        ));

        // Menu items
        let pad_x = 12.0;
        for (idx, item) in self.items.iter().enumerate() {
            let item_y = bounds.y + 2.0 + idx as f32 * self.item_height;
            let item_rect = Rect::new(
                bounds.x + 1.0,
                item_y,
                bounds.width - 2.0,
                self.item_height,
            );

            // Hover highlight
            if self.hovered_index == Some(idx) {
                output.spline_vertices.extend(create_rect_vertices(
                    &item_rect,
                    theme::ACCENT.with_alpha(0.25).to_array(),
                ));
            }

            // Label text
            let text_y = item_y + (self.item_height - line_height) / 2.0;
            let label_color = if self.hovered_index == Some(idx) {
                theme::TEXT_BRIGHT
            } else {
                theme::TEXT
            };
            output.text_vertices.extend(text_renderer.layout_text(
                &item.label,
                bounds.x + pad_x,
                text_y,
                label_color.to_array(),
            ));

            // Shortcut hint (right-aligned, dimmer)
            if let Some(ref shortcut) = item.shortcut {
                let shortcut_width = text_renderer.measure_text(shortcut);
                let shortcut_x = bounds.right() - pad_x - shortcut_width;
                output.text_vertices.extend(text_renderer.layout_text(
                    shortcut,
                    shortcut_x,
                    text_y,
                    theme::TEXT_MUTED.to_array(),
                ));
            }
        }

        output
    }
}

impl Default for ContextMenu {
    fn default() -> Self {
        Self::new()
    }
}
