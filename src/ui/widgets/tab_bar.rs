//! Tab bar widget - horizontal tabs for switching between open repositories

use crate::input::{EventResponse, InputEvent, MouseButton};
use crate::ui::widget::{
    create_rect_vertices, theme, Widget, WidgetOutput,
};
use crate::ui::{Rect, TextRenderer};

/// Actions emitted by the tab bar
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabAction {
    /// User clicked on a tab to select it
    Select(usize),
    /// User wants to close a tab (x button or middle-click)
    Close(usize),
    /// User wants to open a new tab (clicked "+")
    New,
}

/// Data for a single tab
struct Tab {
    name: String,
}

/// Cached bounds from the last layout pass, used for hit-testing
struct CachedBounds {
    tab_rects: Vec<Rect>,
    close_rects: Vec<Rect>,
    new_rect: Rect,
}

/// A horizontal tab bar displayed at the top of the window
pub struct TabBar {
    tabs: Vec<Tab>,
    active: usize,
    hovered_tab: Option<usize>,
    hovered_close: Option<usize>,
    hovered_new: bool,
    pending_action: Option<TabAction>,
    /// Cached from last layout for hit-testing in handle_event/update_hover
    cached: Option<CachedBounds>,
}

impl TabBar {
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            active: 0,
            hovered_tab: None,
            hovered_close: None,
            hovered_new: false,
            pending_action: None,
            cached: None,
        }
    }

    pub fn add_tab(&mut self, name: String) {
        self.tabs.push(Tab { name });
    }

    pub fn set_active(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
        }
    }

    /// Returns true if any tab, close button, or new button is hovered
    pub fn is_any_hovered(&self) -> bool {
        self.hovered_tab.is_some() || self.hovered_close.is_some() || self.hovered_new
    }

    /// Remove a tab and return the new active index
    pub fn remove_tab(&mut self, index: usize) -> usize {
        if index < self.tabs.len() {
            self.tabs.remove(index);
            if (self.active >= self.tabs.len() || self.active > index) && self.active > 0 {
                self.active -= 1;
            }
        }
        self.active
    }

    /// Take pending action if any
    pub fn take_action(&mut self) -> Option<TabAction> {
        self.pending_action.take()
    }

    /// Preferred height of the tab bar (scaled)
    pub fn height(scale: f32) -> f32 {
        28.0 * scale
    }

    /// Compute individual tab bounds and the "+" button bounds
    fn compute_bounds(&self, bounds: Rect, text_renderer: &TextRenderer) -> CachedBounds {
        let scale = (bounds.height / 28.0).max(1.0);
        let tab_padding = 12.0 * scale;
        let close_size = 14.0 * scale;
        let close_gap = 6.0 * scale;
        let new_button_width = 28.0 * scale;
        let gap = 1.0; // pixel gap between tabs

        let mut tab_rects = Vec::new();
        let mut close_rects = Vec::new();
        let mut x = bounds.x;

        for tab in &self.tabs {
            let text_width = text_renderer.measure_text(&tab.name);
            let tab_width = text_width + tab_padding * 2.0 + close_size + close_gap;
            let tab_rect = Rect::new(x, bounds.y, tab_width, bounds.height);

            let close_rect = Rect::new(
                x + tab_width - tab_padding - close_size,
                bounds.y + (bounds.height - close_size) / 2.0,
                close_size,
                close_size,
            );

            tab_rects.push(tab_rect);
            close_rects.push(close_rect);
            x += tab_width + gap;
        }

        let new_rect = Rect::new(x + 4.0 * scale, bounds.y, new_button_width, bounds.height);
        CachedBounds { tab_rects, close_rects, new_rect }
    }

    /// Update hover state and cache bounds (call before layout in the frame loop)
    pub fn update_hover_with_renderer(&mut self, x: f32, y: f32, bounds: Rect, text_renderer: &TextRenderer) {
        let cached = self.compute_bounds(bounds, text_renderer);
        self.update_hover_from_cached(x, y, bounds, &cached);
        self.cached = Some(cached);
    }

    fn update_hover_from_cached(&mut self, x: f32, y: f32, bounds: Rect, cached: &CachedBounds) {
        self.hovered_tab = None;
        self.hovered_close = None;
        self.hovered_new = false;

        if !bounds.contains(x, y) {
            return;
        }

        // Check close buttons first (they overlap with tabs)
        for (i, close_rect) in cached.close_rects.iter().enumerate() {
            if close_rect.contains(x, y) {
                self.hovered_close = Some(i);
                self.hovered_tab = Some(i); // tab is also hovered
                return;
            }
        }

        // Check tab rects
        for (i, tab_rect) in cached.tab_rects.iter().enumerate() {
            if tab_rect.contains(x, y) {
                self.hovered_tab = Some(i);
                return;
            }
        }

        // Check new button
        if cached.new_rect.contains(x, y) {
            self.hovered_new = true;
        }
    }
}

impl Widget for TabBar {
    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        match event {
            InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } => {
                if !bounds.contains(*x, *y) {
                    return EventResponse::Ignored;
                }
                // Use cached bounds for hit-testing
                if let Some(ref cached) = self.cached {
                    // Check close buttons first
                    for (i, close_rect) in cached.close_rects.iter().enumerate() {
                        if close_rect.contains(*x, *y) {
                            self.pending_action = Some(TabAction::Close(i));
                            return EventResponse::Consumed;
                        }
                    }
                    // Check new tab button
                    if cached.new_rect.contains(*x, *y) {
                        self.pending_action = Some(TabAction::New);
                        return EventResponse::Consumed;
                    }
                    // Check tab selection
                    for (i, tab_rect) in cached.tab_rects.iter().enumerate() {
                        if tab_rect.contains(*x, *y) && i != self.active {
                            self.pending_action = Some(TabAction::Select(i));
                            return EventResponse::Consumed;
                        }
                    }
                }
                EventResponse::Consumed // clicked in tab bar area
            }
            InputEvent::MouseDown { button: MouseButton::Middle, x, y, .. } => {
                if !bounds.contains(*x, *y) {
                    return EventResponse::Ignored;
                }
                if let Some(ref cached) = self.cached {
                    for (i, tab_rect) in cached.tab_rects.iter().enumerate() {
                        if tab_rect.contains(*x, *y) {
                            self.pending_action = Some(TabAction::Close(i));
                            return EventResponse::Consumed;
                        }
                    }
                }
                EventResponse::Ignored
            }
            _ => EventResponse::Ignored,
        }
    }

    fn update_hover(&mut self, x: f32, y: f32, bounds: Rect) {
        // Use cached bounds if available
        if let Some(ref cached) = self.cached {
            // Need to re-borrow due to lifetime - clone the data we need
            let tab_rects = cached.tab_rects.clone();
            let close_rects = cached.close_rects.clone();
            let new_rect = cached.new_rect;
            let temp = CachedBounds { tab_rects, close_rects, new_rect };
            self.update_hover_from_cached(x, y, bounds, &temp);
        }
    }

    fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        if self.tabs.is_empty() {
            return output;
        }

        // Background
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            theme::SURFACE.to_array(),
        ));

        let cached = self.compute_bounds(bounds, text_renderer);
        let line_height = text_renderer.line_height();

        for (i, (tab_rect, close_rect)) in cached.tab_rects.iter().zip(cached.close_rects.iter()).enumerate() {
            let is_active = i == self.active;
            let is_hovered = self.hovered_tab == Some(i);
            let is_close_hovered = self.hovered_close == Some(i);

            // Tab background
            let bg = if is_active {
                theme::SURFACE_RAISED
            } else if is_hovered {
                theme::SURFACE_HOVER
            } else {
                theme::SURFACE
            };
            output.spline_vertices.extend(create_rect_vertices(tab_rect, bg.to_array()));

            // Active tab accent line at bottom
            if is_active {
                let accent_rect = Rect::new(
                    tab_rect.x,
                    tab_rect.bottom() - 2.0,
                    tab_rect.width,
                    2.0,
                );
                output.spline_vertices.extend(create_rect_vertices(
                    &accent_rect,
                    theme::ACCENT.to_array(),
                ));
            }

            // Tab name
            let text_color = if is_active {
                theme::TEXT_BRIGHT
            } else if is_hovered {
                theme::TEXT
            } else {
                theme::TEXT_MUTED
            };
            let text_y = tab_rect.y + (tab_rect.height - line_height) / 2.0;
            let scale = (bounds.height / 28.0).max(1.0);
            let text_x = tab_rect.x + 12.0 * scale;
            output.text_vertices.extend(text_renderer.layout_text(
                &self.tabs[i].name,
                text_x,
                text_y,
                text_color.to_array(),
            ));

            // Close button (visible when hovered or active)
            if is_active || is_hovered {
                let close_color = if is_close_hovered {
                    theme::TEXT_BRIGHT
                } else {
                    theme::TEXT_MUTED
                };
                let cx = close_rect.x + (close_rect.width - text_renderer.measure_text("x")) / 2.0;
                let cy = close_rect.y + (close_rect.height - line_height) / 2.0;
                output.text_vertices.extend(text_renderer.layout_text(
                    "x",
                    cx,
                    cy,
                    close_color.to_array(),
                ));

                if is_close_hovered {
                    output.spline_vertices.extend(create_rect_vertices(
                        close_rect,
                        theme::SURFACE_HOVER.with_alpha(0.5).to_array(),
                    ));
                }
            }
        }

        // "+" new tab button
        let new_bg = if self.hovered_new {
            theme::SURFACE_HOVER
        } else {
            theme::SURFACE
        };
        output.spline_vertices.extend(create_rect_vertices(&cached.new_rect, new_bg.to_array()));
        let plus_color = if self.hovered_new {
            theme::TEXT_BRIGHT
        } else {
            theme::TEXT_MUTED
        };
        let plus_x = cached.new_rect.x + (cached.new_rect.width - text_renderer.measure_text("+")) / 2.0;
        let plus_y = cached.new_rect.y + (cached.new_rect.height - line_height) / 2.0;
        output.text_vertices.extend(text_renderer.layout_text(
            "+",
            plus_x,
            plus_y,
            plus_color.to_array(),
        ));

        output
    }
}
