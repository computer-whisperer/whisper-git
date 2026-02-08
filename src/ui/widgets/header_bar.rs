//! Header bar widget - repository name, branch, action buttons

use crate::input::{InputEvent, EventResponse, MouseButton};
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
            commit_button: Button::new("Commit"),
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

        // Help and settings clicks
        match event {
            InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } => {
                if help_bounds.contains(*x, *y) {
                    self.pending_action = Some(HeaderAction::Help);
                    return EventResponse::Consumed;
                }
                if settings_bounds.contains(*x, *y) {
                    self.pending_action = Some(HeaderAction::Settings);
                    return EventResponse::Consumed;
                }
            }
            _ => {}
        }

        EventResponse::Ignored
    }

    fn update_hover(&mut self, x: f32, y: f32, bounds: Rect) {
        let (fetch_bounds, pull_bounds, push_bounds, commit_bounds, _, _) = self.button_bounds(bounds);
        self.fetch_button.update_hover(x, y, fetch_bounds);
        self.pull_button.update_hover(x, y, pull_bounds);
        self.push_button.update_hover(x, y, push_bounds);
        self.commit_button.update_hover(x, y, commit_bounds);
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

        // Fetch button
        let fetch_btn = Button::new(if self.fetching { "..." } else { "Fetch" });
        output.extend(fetch_btn.layout(text_renderer, fetch_bounds));

        // Pull button with badge
        let pull_label = if self.pulling {
            "...".to_string()
        } else if self.behind > 0 {
            format!("Pull (-{})", self.behind)
        } else {
            "Pull".to_string()
        };
        let pull_btn = Button::new(pull_label);
        output.extend(pull_btn.layout(text_renderer, pull_bounds));

        // Push button with badge
        let push_label = if self.pushing {
            "...".to_string()
        } else if self.ahead > 0 {
            format!("Push (+{})", self.ahead)
        } else {
            "Push".to_string()
        };
        let push_btn = Button::new(push_label);
        output.extend(push_btn.layout(text_renderer, push_bounds));

        // Commit button (highlighted when has staged changes)
        let commit_btn = if self.has_staged {
            Button::new("Commit").primary()
        } else {
            Button::new("Commit")
        };
        output.extend(commit_btn.layout(text_renderer, commit_bounds));

        // Help button - icon style
        let help_y = help_bounds.y + (help_bounds.height - line_height) / 2.0;
        output.spline_vertices.extend(create_rect_vertices(
            &help_bounds,
            theme::SURFACE.to_array(),
        ));
        use crate::ui::widget::create_rect_outline_vertices;
        output.spline_vertices.extend(create_rect_outline_vertices(
            &help_bounds,
            theme::BORDER.to_array(),
            1.0,
        ));
        output.text_vertices.extend(text_renderer.layout_text(
            "?",
            help_bounds.x + (help_bounds.width - text_renderer.char_width()) / 2.0,
            help_y,
            theme::TEXT_MUTED.to_array(),
        ));

        // Settings button - icon style
        let settings_y = settings_bounds.y + (settings_bounds.height - line_height) / 2.0;
        output.spline_vertices.extend(create_rect_vertices(
            &settings_bounds,
            theme::SURFACE.to_array(),
        ));
        output.spline_vertices.extend(create_rect_outline_vertices(
            &settings_bounds,
            theme::BORDER.to_array(),
            1.0,
        ));
        output.text_vertices.extend(text_renderer.layout_text(
            "=",
            settings_bounds.x + (settings_bounds.width - text_renderer.char_width()) / 2.0,
            settings_y,
            theme::TEXT_MUTED.to_array(),
        ));

        output
    }
}
