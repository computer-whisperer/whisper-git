//! Header bar widget - repository name, branch, action buttons

use crate::input::{InputEvent, EventResponse};
use crate::ui::{Rect, TextRenderer};
use crate::ui::widget::{Widget, WidgetId, WidgetOutput, create_rect_vertices, theme};
use crate::ui::widgets::Button;

/// Actions that can be triggered from the header bar
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderAction {
    Fetch,
    Pull,
    Push,
    Commit,
    Help,
    Settings,
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
        // Fetch button label
        self.fetch_button.label = if self.fetching {
            "...".to_string()
        } else {
            "Fetch".to_string()
        };

        // Pull button label with behind badge
        self.pull_button.label = if self.pulling {
            "...".to_string()
        } else if self.behind > 0 {
            format!("Pull (-{})", self.behind)
        } else {
            "Pull".to_string()
        };

        // Push button label with ahead badge
        self.push_button.label = if self.pushing {
            "...".to_string()
        } else if self.ahead > 0 {
            format!("Push (+{})", self.ahead)
        } else {
            "Push".to_string()
        };

        // Commit button: always primary style (blue accent)
        self.commit_button.background = theme::ACCENT;
        self.commit_button.hover_background = crate::ui::Color::rgba(0.35, 0.70, 1.0, 1.0);
        self.commit_button.pressed_background = crate::ui::Color::rgba(0.20, 0.55, 0.85, 1.0);
        self.commit_button.text_color = theme::TEXT_BRIGHT;
        self.commit_button.border_color = None;
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

        // Repository name
        let repo_x = bounds.x + 16.0;
        output.text_vertices.extend(text_renderer.layout_text(
            &self.repo_name,
            repo_x,
            text_y,
            theme::TEXT.to_array(),
        ));

        // Separator
        let sep_x = repo_x + text_renderer.measure_text(&self.repo_name) + 16.0;
        output.text_vertices.extend(text_renderer.layout_text(
            "|",
            sep_x,
            text_y,
            theme::TEXT_MUTED.to_array(),
        ));

        // Branch name
        let branch_x = sep_x + 24.0;
        output.text_vertices.extend(text_renderer.layout_text(
            &self.branch_name,
            branch_x,
            text_y,
            theme::BRANCH_FEATURE.to_array(),
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
