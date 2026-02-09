//! Header bar widget - repository name, branch, action buttons

use crate::input::{InputEvent, EventResponse};
use crate::ui::{Rect, TextRenderer};
use crate::ui::widget::{Widget, WidgetId, WidgetOutput, create_rect_vertices, create_rounded_rect_vertices, theme};
use crate::ui::widgets::Button;

/// Actions that can be triggered from the header bar
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeaderAction {
    Fetch,
    Pull,
    Push,
    Commit,
    Help,
    Settings,
    /// Breadcrumb click: navigate to the given depth (0 = root)
    BreadcrumbNav(usize),
    /// Close button in breadcrumb mode: return to root
    BreadcrumbClose,
}

/// Header bar widget displaying repo info and action buttons
#[allow(dead_code)]
pub struct HeaderBar {
    id: WidgetId,
    /// Repository name
    pub repo_name: String,
    /// Current branch name
    pub branch_name: String,
    /// Commits ahead of remote
    pub ahead: usize,
    /// Commits behind remote
    pub behind: usize,
    /// Whether a fetch operation is in progress
    pub fetching: bool,
    /// Whether a pull operation is in progress
    pub pulling: bool,
    /// Whether a push operation is in progress
    pub pushing: bool,
    /// Whether there are staged changes (highlights commit button)
    pub has_staged: bool,
    /// Pending action (set after button click)
    pending_action: Option<HeaderAction>,
    /// Button states (for hover/press tracking)
    fetch_button: Button,
    pull_button: Button,
    push_button: Button,
    commit_button: Button,
    help_button: Button,
    settings_button: Button,
    /// Breadcrumb segments: empty = normal mode, non-empty = submodule drill-down
    /// First segment is the root repo name, last is the current submodule
    pub breadcrumb_segments: Vec<String>,
    /// Which breadcrumb segment is hovered (for click highlighting)
    breadcrumb_hovered: Option<usize>,
    /// Close button for breadcrumb mode [✕]
    close_button: Button,
    /// Cached bounds for each breadcrumb segment (for hit testing)
    breadcrumb_segment_bounds: Vec<Rect>,
}

impl HeaderBar {
    pub fn new() -> Self {
        Self {
            id: WidgetId::new(),
            repo_name: String::new(),
            branch_name: String::new(),
            ahead: 0,
            behind: 0,
            fetching: false,
            pulling: false,
            pushing: false,
            has_staged: false,
            pending_action: None,
            fetch_button: Button::new("Fetch"),
            pull_button: Button::new("Pull"),
            push_button: Button::new("Push"),
            commit_button: Button::new("Commit").primary(),
            help_button: Button::new("?").ghost(),
            settings_button: Button::new("\u{2261}").ghost(),
            breadcrumb_segments: Vec::new(),
            breadcrumb_hovered: None,
            close_button: Button::new("\u{2715}").ghost(), // ✕
            breadcrumb_segment_bounds: Vec::new(),
        }
    }

    /// Update repository information
    pub fn set_repo_info(&mut self, repo_name: String, branch_name: String, ahead: usize, behind: usize) {
        self.repo_name = repo_name;
        self.branch_name = branch_name;
        self.ahead = ahead;
        self.behind = behind;
    }

    /// Check if an action was triggered and clear it
    pub fn take_action(&mut self) -> Option<HeaderAction> {
        self.pending_action.take()
    }

    /// Sync button labels and styles to current header state.
    /// Call this before layout so the stored buttons render the correct text.
    pub fn update_button_state(&mut self) {
        // Fetch button label (~ as refresh icon, ASCII-safe for font atlas)
        self.fetch_button.label = if self.fetching {
            "...".to_string()
        } else {
            "~ Fetch".to_string()
        };

        // Pull button label with behind badge (v as down-arrow icon)
        self.pull_button.label = if self.pulling {
            "...".to_string()
        } else if self.behind > 0 {
            format!("v Pull (-{})", self.behind)
        } else {
            "v Pull".to_string()
        };

        // Push button label with ahead badge (^ as up-arrow icon)
        self.push_button.label = if self.pushing {
            "...".to_string()
        } else if self.ahead > 0 {
            format!("^ Push (+{})", self.ahead)
        } else {
            "^ Push".to_string()
        };

        // Commit button: always primary style (blue accent)
        self.commit_button.background = theme::ACCENT;
        self.commit_button.hover_background = crate::ui::Color::rgba(0.35, 0.70, 1.0, 1.0);
        self.commit_button.pressed_background = crate::ui::Color::rgba(0.20, 0.55, 0.85, 1.0);
        self.commit_button.text_color = theme::TEXT_BRIGHT;
        self.commit_button.border_color = None;
    }

    /// Compute bounds for the breadcrumb close button [✕]
    fn close_button_bounds(&self, bounds: Rect, scale: f32) -> Rect {
        let button_height = bounds.height - 8.0 * scale;
        let button_y = bounds.y + 4.0 * scale;
        let icon_w = 28.0 * scale;

        // Position after the last breadcrumb segment text
        // We use a fixed position relative to the measured breadcrumb text width
        let mut x = bounds.x + 16.0;
        // Approximate: we can't call text_renderer here, so use stored bounds
        if let Some(last_bound) = self.breadcrumb_segment_bounds.last() {
            x = last_bound.right() + 8.0;
        }

        Rect::new(x, button_y, icon_w, button_height)
    }

    /// Update breadcrumb segment bounds (call before layout with text_renderer access)
    pub fn update_breadcrumb_bounds(&mut self, text_renderer: &crate::ui::TextRenderer, bounds: Rect) {
        self.breadcrumb_segment_bounds.clear();
        if self.breadcrumb_segments.is_empty() {
            return;
        }

        let line_height = text_renderer.line_height();
        let text_y = bounds.y + (bounds.height - line_height) / 2.0;
        let separator = " > ";
        let sep_w = text_renderer.measure_text(separator);
        let mut x = bounds.x + 16.0;

        for (i, segment) in self.breadcrumb_segments.iter().enumerate() {
            let seg_w = text_renderer.measure_text(segment);
            self.breadcrumb_segment_bounds.push(Rect::new(x, text_y, seg_w, line_height));
            x += seg_w;
            if i < self.breadcrumb_segments.len() - 1 {
                x += sep_w;
            }
        }
    }

    /// Compute button bounds within the header (scale-aware)
    fn button_bounds(&self, bounds: Rect) -> (Rect, Rect, Rect, Rect, Rect, Rect) {
        // Derive scale from header height (which is already scaled by ScreenLayout)
        let scale = (bounds.height / 32.0).max(1.0);
        let button_height = bounds.height - 8.0 * scale;
        let button_y = bounds.y + 4.0 * scale;
        let button_width = 110.0 * scale;
        let icon_button_width = 32.0 * scale;
        let gap = 8.0 * scale;

        // Right-aligned buttons: [?][=] at far right
        let settings_x = bounds.right() - icon_button_width - gap;
        let help_x = settings_x - icon_button_width - gap;

        // Action buttons: [Fetch] [Pull] [Push] [Commit] before help/settings
        let commit_x = help_x - button_width - gap * 2.0;
        let push_x = commit_x - button_width - gap;
        let pull_x = push_x - button_width - gap;
        let fetch_x = pull_x - button_width - gap;

        (
            Rect::new(fetch_x, button_y, button_width, button_height),
            Rect::new(pull_x, button_y, button_width, button_height),
            Rect::new(push_x, button_y, button_width, button_height),
            Rect::new(commit_x, button_y, button_width, button_height),
            Rect::new(help_x, button_y, icon_button_width, button_height),
            Rect::new(settings_x, button_y, icon_button_width, button_height),
        )
    }
}

impl Default for HeaderBar {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for HeaderBar {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        let (fetch_bounds, pull_bounds, push_bounds, commit_bounds, help_bounds, settings_bounds) =
            self.button_bounds(bounds);

        // Handle breadcrumb close button and segment clicks
        if !self.breadcrumb_segments.is_empty() {
            let scale = (bounds.height / 32.0).max(1.0);
            let close_bounds = self.close_button_bounds(bounds, scale);
            if self.close_button.handle_event(event, close_bounds).is_consumed() {
                if self.close_button.was_clicked() {
                    self.pending_action = Some(HeaderAction::BreadcrumbClose);
                }
                return EventResponse::Consumed;
            }

            // Check clicks on breadcrumb segments (non-last are clickable)
            if let InputEvent::MouseDown { x, y, .. } = event {
                for (i, seg_bounds) in self.breadcrumb_segment_bounds.iter().enumerate() {
                    if i < self.breadcrumb_segments.len() - 1 && seg_bounds.contains(*x, *y) {
                        self.pending_action = Some(HeaderAction::BreadcrumbNav(i));
                        return EventResponse::Consumed;
                    }
                }
            }
        }

        // Handle button events
        if self.fetch_button.handle_event(event, fetch_bounds).is_consumed() {
            if self.fetch_button.was_clicked() {
                self.pending_action = Some(HeaderAction::Fetch);
            }
            return EventResponse::Consumed;
        }

        if self.pull_button.handle_event(event, pull_bounds).is_consumed() {
            if self.pull_button.was_clicked() {
                self.pending_action = Some(HeaderAction::Pull);
            }
            return EventResponse::Consumed;
        }

        if self.push_button.handle_event(event, push_bounds).is_consumed() {
            if self.push_button.was_clicked() {
                self.pending_action = Some(HeaderAction::Push);
            }
            return EventResponse::Consumed;
        }

        if self.commit_button.handle_event(event, commit_bounds).is_consumed() {
            if self.commit_button.was_clicked() {
                self.pending_action = Some(HeaderAction::Commit);
            }
            return EventResponse::Consumed;
        }

        if self.help_button.handle_event(event, help_bounds).is_consumed() {
            if self.help_button.was_clicked() {
                self.pending_action = Some(HeaderAction::Help);
            }
            return EventResponse::Consumed;
        }

        if self.settings_button.handle_event(event, settings_bounds).is_consumed() {
            if self.settings_button.was_clicked() {
                self.pending_action = Some(HeaderAction::Settings);
            }
            return EventResponse::Consumed;
        }

        EventResponse::Ignored
    }

    fn update_hover(&mut self, x: f32, y: f32, bounds: Rect) {
        let (fetch_bounds, pull_bounds, push_bounds, commit_bounds, help_bounds, settings_bounds) = self.button_bounds(bounds);
        self.fetch_button.update_hover(x, y, fetch_bounds);
        self.pull_button.update_hover(x, y, pull_bounds);
        self.push_button.update_hover(x, y, push_bounds);
        self.commit_button.update_hover(x, y, commit_bounds);
        self.help_button.update_hover(x, y, help_bounds);
        self.settings_button.update_hover(x, y, settings_bounds);

        // Breadcrumb hover tracking
        if !self.breadcrumb_segments.is_empty() {
            let scale = (bounds.height / 32.0).max(1.0);
            self.close_button.update_hover(x, y, self.close_button_bounds(bounds, scale));

            self.breadcrumb_hovered = None;
            for (i, seg_bounds) in self.breadcrumb_segment_bounds.iter().enumerate() {
                if i < self.breadcrumb_segments.len() - 1 && seg_bounds.contains(x, y) {
                    self.breadcrumb_hovered = Some(i);
                    break;
                }
            }
        }
    }

    fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        // Background - elevated surface for header prominence
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            theme::SURFACE_RAISED.to_array(),
        ));

        let line_height = text_renderer.line_height();
        let text_y = bounds.y + (bounds.height - line_height) / 2.0;

        // Determine where the branch pill starts (depends on breadcrumb vs normal mode)
        let branch_pill_x;

        if self.breadcrumb_segments.is_empty() {
            // Normal mode: repo name + separator
            let repo_x = bounds.x + 16.0;
            output.text_vertices.extend(text_renderer.layout_text(
                &self.repo_name,
                repo_x,
                text_y,
                theme::TEXT.to_array(),
            ));

            let sep_x = repo_x + text_renderer.measure_text(&self.repo_name) + 12.0;
            let sep_height = line_height * 0.8;
            let sep_y = bounds.y + (bounds.height - sep_height) / 2.0;
            output.spline_vertices.extend(create_rect_vertices(
                &Rect::new(sep_x, sep_y, 1.0, sep_height),
                theme::BORDER.to_array(),
            ));

            branch_pill_x = sep_x + 12.0;
        } else {
            // Breadcrumb mode: segment > segment > ... + close button
            let scale = (bounds.height / 32.0).max(1.0);
            let mut x = bounds.x + 16.0;
            let separator = " > ";
            let sep_w = text_renderer.measure_text(separator);

            // We need to write segment bounds into a mutable ref via interior mutability workaround
            // Since layout takes &self, we'll store bounds via the segment_bounds Vec
            // which was pre-computed. For rendering we just draw based on current positions.
            let last_idx = self.breadcrumb_segments.len() - 1;

            for (i, segment) in self.breadcrumb_segments.iter().enumerate() {
                let is_last = i == last_idx;
                let is_hovered = self.breadcrumb_hovered == Some(i);

                let color = if is_last {
                    theme::TEXT_BRIGHT.to_array()
                } else if is_hovered {
                    theme::TEXT.to_array()
                } else {
                    theme::TEXT_MUTED.to_array()
                };

                output.text_vertices.extend(text_renderer.layout_text(
                    segment,
                    x,
                    text_y,
                    color,
                ));

                // Underline hovered non-last segments for clickability affordance
                if !is_last && is_hovered {
                    let seg_w = text_renderer.measure_text(segment);
                    output.spline_vertices.extend(create_rect_vertices(
                        &Rect::new(x, text_y + line_height - 1.0, seg_w, 1.0),
                        theme::TEXT_MUTED.to_array(),
                    ));
                }

                x += text_renderer.measure_text(segment);

                if !is_last {
                    output.text_vertices.extend(text_renderer.layout_text(
                        separator,
                        x,
                        text_y,
                        theme::TEXT_MUTED.to_array(),
                    ));
                    x += sep_w;
                }
            }

            // Close button [✕]
            let close_bounds = self.close_button_bounds(bounds, scale);
            output.extend(self.close_button.layout(text_renderer, close_bounds));

            // Separator before branch pill
            let sep_x = close_bounds.right() + 8.0;
            let sep_height = line_height * 0.8;
            let sep_y = bounds.y + (bounds.height - sep_height) / 2.0;
            output.spline_vertices.extend(create_rect_vertices(
                &Rect::new(sep_x, sep_y, 1.0, sep_height),
                theme::BORDER.to_array(),
            ));

            branch_pill_x = sep_x + 12.0;
        }

        // Branch name inside a tinted pill
        let branch_text_w = text_renderer.measure_text(&self.branch_name);
        let pill_pad_h = 10.0;
        let pill_pad_v = 3.0;
        let pill_h = line_height + pill_pad_v * 2.0;
        let pill_w = branch_text_w + pill_pad_h * 2.0;
        let pill_y = bounds.y + (bounds.height - pill_h) / 2.0;
        let pill_rect = Rect::new(branch_pill_x, pill_y, pill_w, pill_h);
        let pill_radius = pill_h / 2.0;

        output.spline_vertices.extend(create_rounded_rect_vertices(
            &pill_rect,
            theme::ACCENT.with_alpha(0.15).to_array(),
            pill_radius,
        ));

        let branch_text_x = branch_pill_x + pill_pad_h;
        let branch_text_y = pill_y + pill_pad_v;
        output.text_vertices.extend(text_renderer.layout_text(
            &self.branch_name,
            branch_text_x,
            branch_text_y,
            theme::ACCENT.to_array(),
        ));

        // Button bounds
        let (fetch_bounds, pull_bounds, push_bounds, commit_bounds, help_bounds, settings_bounds) =
            self.button_bounds(bounds);

        // Render stored buttons (preserves hover/press state from handle_event)
        output.extend(self.fetch_button.layout(text_renderer, fetch_bounds));
        output.extend(self.pull_button.layout(text_renderer, pull_bounds));
        output.extend(self.push_button.layout(text_renderer, push_bounds));
        output.extend(self.commit_button.layout(text_renderer, commit_bounds));

        // Help and Settings buttons (ghost style - rendered via Button widget)
        output.extend(self.help_button.layout(text_renderer, help_bounds));
        output.extend(self.settings_button.layout(text_renderer, settings_bounds));

        // Drop shadow below header: 4 strips fading from rgba(0,0,0,0.15) to transparent
        let shadow_strip_height = 2.0;
        for i in 0..4u32 {
            let alpha = 0.15 * (1.0 - i as f32 / 4.0);
            let strip_y = bounds.bottom() + i as f32 * shadow_strip_height;
            let strip = Rect::new(bounds.x, strip_y, bounds.width, shadow_strip_height);
            output.spline_vertices.extend(create_rect_vertices(
                &strip,
                [0.0, 0.0, 0.0, alpha],
            ));
        }

        output
    }
}
