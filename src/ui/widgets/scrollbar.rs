//! Scrollbar widget - vertical scrollbar with track, thumb, and drag support

use crate::input::{EventResponse, InputEvent, MouseButton};
use crate::ui::widget::{create_rect_vertices, theme, WidgetOutput};
use crate::ui::Rect;

/// Actions produced by the scrollbar
#[derive(Clone, Debug)]
pub enum ScrollAction {
    /// Scroll to a normalized position (0.0 = top, 1.0 = bottom)
    ScrollTo(f32),
}

/// A vertical scrollbar with track and draggable thumb
pub struct Scrollbar {
    /// Total number of content items
    total_items: usize,
    /// Number of visible items
    visible_items: usize,
    /// Current scroll offset (in items)
    scroll_offset: usize,
    /// Whether the thumb is being dragged
    dragging: bool,
    /// Y offset within thumb when drag started
    drag_offset: f32,
    /// Whether the mouse is hovering over the scrollbar
    hovered: bool,
    /// Pending action
    pending_action: Option<ScrollAction>,
}

impl Scrollbar {
    pub fn new() -> Self {
        Self {
            total_items: 0,
            visible_items: 0,
            scroll_offset: 0,
            dragging: false,
            drag_offset: 0.0,
            hovered: false,
            pending_action: None,
        }
    }

    /// Update the content dimensions and scroll position
    pub fn set_content(&mut self, total_items: usize, visible_items: usize, scroll_offset: usize) {
        self.total_items = total_items;
        self.visible_items = visible_items;
        self.scroll_offset = scroll_offset;
    }

    /// Whether the scrollbar should be visible (content exceeds visible area)
    pub fn is_visible(&self) -> bool {
        self.total_items > self.visible_items && self.visible_items > 0
    }

    /// Take the pending action
    pub fn take_action(&mut self) -> Option<ScrollAction> {
        self.pending_action.take()
    }

    /// Compute the thumb rect within the given track bounds
    fn thumb_rect(&self, bounds: &Rect) -> Rect {
        if self.total_items == 0 || self.visible_items >= self.total_items {
            return *bounds;
        }

        let ratio = self.visible_items as f32 / self.total_items as f32;
        let thumb_height = (bounds.height * ratio).max(20.0);
        let scrollable_range = bounds.height - thumb_height;
        let max_offset = self.total_items.saturating_sub(self.visible_items);
        let scroll_ratio = if max_offset > 0 {
            self.scroll_offset as f32 / max_offset as f32
        } else {
            0.0
        };
        let thumb_y = bounds.y + scrollable_range * scroll_ratio;

        Rect::new(bounds.x, thumb_y, bounds.width, thumb_height)
    }

    /// Convert a y position in the track to a normalized scroll position
    fn y_to_scroll_offset(&self, y: f32, bounds: &Rect) -> usize {
        if self.total_items <= self.visible_items {
            return 0;
        }

        let thumb_height = {
            let ratio = self.visible_items as f32 / self.total_items as f32;
            (bounds.height * ratio).max(20.0)
        };
        let scrollable_range = bounds.height - thumb_height;
        if scrollable_range <= 0.0 {
            return 0;
        }

        let clamped_y = (y - bounds.y).clamp(0.0, scrollable_range);
        let ratio = clamped_y / scrollable_range;
        let max_offset = self.total_items.saturating_sub(self.visible_items);
        (ratio * max_offset as f32).round() as usize
    }

    /// Handle input events
    pub fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        if !self.is_visible() {
            return EventResponse::Ignored;
        }

        match event {
            InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } => {
                if bounds.contains(*x, *y) {
                    let thumb = self.thumb_rect(&bounds);
                    if thumb.contains(*x, *y) {
                        // Start dragging the thumb
                        self.dragging = true;
                        self.drag_offset = *y - thumb.y;
                    } else {
                        // Click on track - jump to that position
                        let thumb_height = thumb.height;
                        let target_y = *y - thumb_height / 2.0;
                        let new_offset = self.y_to_scroll_offset(target_y, &bounds);
                        self.pending_action = Some(ScrollAction::ScrollTo(
                            new_offset as f32 / self.total_items.saturating_sub(self.visible_items).max(1) as f32,
                        ));
                    }
                    return EventResponse::Consumed;
                }
            }
            InputEvent::MouseUp { button: MouseButton::Left, .. } => {
                if self.dragging {
                    self.dragging = false;
                    return EventResponse::Consumed;
                }
            }
            InputEvent::MouseMove { x, y, .. } => {
                self.hovered = bounds.contains(*x, *y);
                if self.dragging {
                    let target_y = *y - self.drag_offset;
                    let new_offset = self.y_to_scroll_offset(target_y, &bounds);
                    let max_offset = self.total_items.saturating_sub(self.visible_items).max(1);
                    self.pending_action = Some(ScrollAction::ScrollTo(
                        new_offset as f32 / max_offset as f32,
                    ));
                    return EventResponse::Consumed;
                }
            }
            _ => {}
        }

        EventResponse::Ignored
    }

    /// Render the scrollbar
    pub fn layout(&self, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        if !self.is_visible() {
            return output;
        }

        // Track background
        let track_color = theme::SURFACE.with_alpha(0.5);
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            track_color.to_array(),
        ));

        // Thumb
        let thumb = self.thumb_rect(&bounds);
        let thumb_color = if self.dragging {
            theme::ACCENT.with_alpha(0.7)
        } else if self.hovered {
            theme::ACCENT.with_alpha(0.45)
        } else {
            theme::TEXT_MUTED.with_alpha(0.3)
        };

        // Rounded corners via inset for visual softness
        let thumb_inset = Rect::new(
            thumb.x + 1.0,
            thumb.y + 1.0,
            thumb.width - 2.0,
            thumb.height - 2.0,
        );
        output.spline_vertices.extend(create_rect_vertices(
            &thumb_inset,
            thumb_color.to_array(),
        ));

        output
    }
}

impl Default for Scrollbar {
    fn default() -> Self {
        Self::new()
    }
}
