//! Header bar widget - repository name, branch, action buttons

use crate::input::{InputEvent, EventResponse};
use crate::ui::{Rect, TextRenderer};
use crate::ui::widget::{Widget, WidgetOutput, create_rect_vertices, create_rounded_rect_vertices, create_arc_vertices, theme};
use crate::ui::widgets::Button;

/// Actions that can be triggered from the header bar
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeaderAction {
    Fetch,
    Pull,
    PullRebase,
    Push,
    Commit,
    Help,
    Settings,
    /// Breadcrumb click: navigate to the given depth (0 = root)
    BreadcrumbNav(usize),
    /// Close button in breadcrumb mode: return to root
    BreadcrumbClose,
    /// Abort an in-progress git operation (merge, rebase, etc.)
    AbortOperation,
}

/// Header bar widget displaying repo info and action buttons
pub struct HeaderBar {
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
    /// Operation state label (e.g. "MERGE IN PROGRESS"), None when clean
    pub operation_state_label: Option<&'static str>,
    /// Abort button for in-progress operations
    abort_button: Button,
    /// Cached abort button bounds (computed during update_breadcrumb_bounds)
    abort_button_bounds: Option<Rect>,
    /// Whether shift was held during the last pull button click (for pull --rebase)
    pull_shift_held: bool,
    /// Label for a generic async operation in progress (e.g. "Merging...", "Rebasing...")
    /// When set, renders a spinning indicator in the header next to the branch pill.
    pub generic_op_label: Option<String>,
    /// Tracking remote name (e.g. "origin"). Shown next to branch pill when non-empty.
    pub remote_name: String,
}

impl HeaderBar {
    pub fn new() -> Self {
        Self {
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
            operation_state_label: None,
            abort_button: Button::new("Abort"),
            abort_button_bounds: None,
            pull_shift_held: false,
            generic_op_label: None,
            remote_name: String::new(),
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
    /// `elapsed` is seconds since app start, used for animated dot cycling.
    pub fn update_button_state(&mut self, elapsed: f32) {
        // Animated dots: cycles 1..3 dots every ~1.2s
        let dot_count = ((elapsed * 2.5) as usize % 3) + 1;
        let dots: String = ".".repeat(dot_count);

        // Remote name suffix for button labels (e.g. " origin")
        let remote_suffix = if self.remote_name.is_empty() {
            String::new()
        } else {
            format!(" {}", self.remote_name)
        };

        // Fetch button label (no prefix — Roboto lacks a refresh/circular arrow glyph)
        self.fetch_button.label = if self.fetching {
            format!("Fetching{}{}", remote_suffix, dots)
        } else {
            format!("Fetch{}", remote_suffix)
        };

        // Pull button label with behind badge (↓ down arrow)
        self.pull_button.label = if self.pulling {
            format!("\u{2193} Pulling{}{}", remote_suffix, dots)
        } else if self.behind > 0 {
            format!("\u{2193} Pull{} (-{})", remote_suffix, self.behind)
        } else {
            format!("\u{2193} Pull{}", remote_suffix)
        };

        // Push button label with ahead badge (↑ up arrow)
        self.push_button.label = if self.pushing {
            format!("\u{2191} Pushing{}{}", remote_suffix, dots)
        } else if self.ahead > 0 {
            format!("\u{2191} Push{} (+{})", remote_suffix, self.ahead)
        } else {
            format!("\u{2191} Push{}", remote_suffix)
        };

        // Fetch/Pull/Push buttons: slightly raised above the header's SURFACE_RAISED background
        // so they're visually distinct and look clickable (header bg is also SURFACE_RAISED).
        let btn_bg = crate::ui::Color::rgba(0.20, 0.22, 0.28, 1.0);       // #333847 — visible step above header
        let btn_hover = crate::ui::Color::rgba(0.24, 0.27, 0.34, 1.0);    // #3d4557 — brighter on hover
        let btn_pressed = crate::ui::Color::rgba(0.15, 0.17, 0.22, 1.0);  // #262b38 — dimmer on press
        for btn in [&mut self.fetch_button, &mut self.pull_button, &mut self.push_button] {
            btn.background = btn_bg;
            btn.hover_background = btn_hover;
            btn.pressed_background = btn_pressed;
            btn.text_color = theme::TEXT;
            btn.border_color = Some(theme::BORDER);
        }

        // Abort button: amber/warning color scheme
        self.abort_button.background = crate::ui::Color::rgba(0.35, 0.25, 0.10, 1.0);
        self.abort_button.hover_background = crate::ui::Color::rgba(0.45, 0.32, 0.12, 1.0);
        self.abort_button.pressed_background = crate::ui::Color::rgba(0.30, 0.20, 0.08, 1.0);
        self.abort_button.text_color = crate::ui::Color::rgba(1.0, 0.718, 0.302, 1.0); // amber
        self.abort_button.border_color = Some(crate::ui::Color::rgba(1.0, 0.718, 0.302, 0.5));

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
    pub fn update_breadcrumb_bounds(&mut self, text_renderer: &TextRenderer, bounds: Rect) {
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

    /// Pre-compute abort button bounds (call from the pre-draw phase with text_renderer access).
    /// `bold_renderer` is used for measuring repo name and branch name (rendered bold in layout).
    pub fn update_abort_bounds(&mut self, bold_renderer: &TextRenderer, bounds: Rect) {
        if self.operation_state_label.is_none() {
            self.abort_button_bounds = None;
            return;
        }
        let scale = (bounds.height / 32.0).max(1.0);
        let button_height = bounds.height - 8.0 * scale;
        let button_y = bounds.y + 4.0 * scale;
        let abort_w = 80.0 * scale;

        // Compute branch pill end position
        let branch_pill_x = if self.breadcrumb_segments.is_empty() {
            if self.repo_name == self.branch_name {
                bounds.x + 16.0
            } else {
                let repo_x = bounds.x + 16.0;
                let repo_w = bold_renderer.measure_text(&self.repo_name);
                let sep_x = repo_x + repo_w + 12.0;
                sep_x + 12.0
            }
        } else if let Some(last_bound) = self.breadcrumb_segment_bounds.last() {
            last_bound.right() + 8.0 + 28.0 * scale + 8.0 + 12.0
        } else {
            bounds.x + 16.0
        };

        let branch_text_w = bold_renderer.measure_text(&self.branch_name);
        let pill_pad_h = 10.0;
        let pill_w = branch_text_w + pill_pad_h * 2.0;

        // Place operation label + abort button after branch pill
        let label_text = self.operation_state_label.unwrap_or("");
        let label_w = bold_renderer.measure_text(label_text);
        let abort_x = branch_pill_x + pill_w + 12.0 + label_w + 8.0;

        self.abort_button_bounds = Some(Rect::new(abort_x, button_y, abort_w, button_height));
    }

    /// Returns true if any interactive element in the header is currently hovered
    /// (buttons, breadcrumb links, close button, abort button).
    pub fn is_any_interactive_hovered(&self) -> bool {
        self.fetch_button.is_hovered()
            || self.pull_button.is_hovered()
            || self.push_button.is_hovered()
            || self.commit_button.is_hovered()
            || self.help_button.is_hovered()
            || self.settings_button.is_hovered()
            || self.close_button.is_hovered()
            || self.abort_button.is_hovered()
            || self.breadcrumb_hovered.is_some()
    }

    /// Compute button bounds within the header (scale-aware)
    fn button_bounds(&self, bounds: Rect) -> (Rect, Rect, Rect, Rect, Rect, Rect) {
        // Derive scale from header height (which is already scaled by ScreenLayout)
        let scale = (bounds.height / 32.0).max(1.0);
        let button_height = bounds.height - 8.0 * scale;
        let button_y = bounds.y + 4.0 * scale;
        let button_width = 130.0 * scale;
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

impl HeaderBar {
    /// Layout with bold text support. Renders branch name and button labels in bold.
    /// `elapsed` is seconds since app start, used for spinning arc and pulsing animations.
    pub fn layout_with_bold(&self, text_renderer: &TextRenderer, bold_renderer: &TextRenderer, bounds: Rect, elapsed: f32) -> WidgetOutput {
        let mut output = WidgetOutput::new();
        let scale = (bounds.height / 32.0).max(1.0);

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
            // Normal mode: repo name in bold + separator
            // Skip repo name when it matches the branch (avoids "main | main" redundancy)
            if self.repo_name == self.branch_name {
                branch_pill_x = bounds.x + 16.0;
            } else {
                let repo_x = bounds.x + 16.0;
                output.bold_text_vertices.extend(bold_renderer.layout_text(
                    &self.repo_name,
                    repo_x,
                    text_y,
                    theme::TEXT.to_array(),
                ));

                let sep_x = repo_x + bold_renderer.measure_text(&self.repo_name) + 12.0;
                let sep_height = line_height * 0.8;
                let sep_y = bounds.y + (bounds.height - sep_height) / 2.0;
                output.spline_vertices.extend(create_rect_vertices(
                    &Rect::new(sep_x, sep_y, 1.0, sep_height),
                    theme::BORDER.to_array(),
                ));

                branch_pill_x = sep_x + 12.0;
            }
        } else {
            // Breadcrumb mode: segment > segment > ... + close button
            let mut x = bounds.x + 16.0;
            let separator = " > ";
            let sep_w = text_renderer.measure_text(separator);

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

                // Last segment (current) in bold, others in regular
                if is_last {
                    output.bold_text_vertices.extend(bold_renderer.layout_text(
                        segment, x, text_y, color,
                    ));
                } else {
                    output.text_vertices.extend(text_renderer.layout_text(
                        segment, x, text_y, color,
                    ));
                }

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
                        separator, x, text_y, theme::TEXT_MUTED.to_array(),
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

        // Branch name inside a tinted pill - bold text
        let branch_text_w = bold_renderer.measure_text(&self.branch_name);
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
        output.bold_text_vertices.extend(bold_renderer.layout_text(
            &self.branch_name,
            branch_text_x,
            branch_text_y,
            theme::ACCENT.to_array(),
        ));

        // Ahead/behind indicators next to the branch pill
        let mut after_pill_x = branch_pill_x + pill_w;
        if self.ahead > 0 || self.behind > 0 {
            after_pill_x += 8.0;
            if self.ahead > 0 {
                let ahead_text = format!("\u{2191}{}", self.ahead);
                output.bold_text_vertices.extend(bold_renderer.layout_text(
                    &ahead_text,
                    after_pill_x,
                    text_y,
                    theme::STATUS_CLEAN.to_array(),
                ));
                after_pill_x += bold_renderer.measure_text(&ahead_text) + 4.0;
            }
            if self.behind > 0 {
                let behind_text = format!("\u{2193}{}", self.behind);
                output.bold_text_vertices.extend(bold_renderer.layout_text(
                    &behind_text,
                    after_pill_x,
                    text_y,
                    theme::STATUS_BEHIND.to_array(),
                ));
                after_pill_x += bold_renderer.measure_text(&behind_text);
            }
            after_pill_x += 4.0;
        }

        // Remote name indicator (e.g. "origin") in muted text after branch pill
        if !self.remote_name.is_empty() {
            let remote_label = format!("{}", self.remote_name);
            let remote_x = after_pill_x + 8.0;
            output.text_vertices.extend(text_renderer.layout_text(
                &remote_label,
                remote_x,
                text_y,
                theme::TEXT_MUTED.to_array(),
            ));
            after_pill_x = remote_x + text_renderer.measure_text(&remote_label);
        }

        // Operation state banner (e.g. "MERGE IN PROGRESS" + Abort button)
        if let Some(label) = self.operation_state_label {
            let label_x = after_pill_x + 12.0;
            let label_color = [1.0, 0.718, 0.302, 1.0]; // amber #FFB74D
            output.bold_text_vertices.extend(bold_renderer.layout_text(
                label, label_x, text_y, label_color,
            ));

            if let Some(abort_bounds) = self.abort_button_bounds {
                output.extend(self.abort_button.layout_with_bold(text_renderer, bold_renderer, abort_bounds));
            }
        }

        // Generic operation indicator (e.g. "Merging..." with spinner)
        // Only show when no operation_state_label is already displayed
        if self.operation_state_label.is_none() {
            if let Some(ref op_label) = self.generic_op_label {
                let indicator_x = after_pill_x + 12.0;
                let spinner_radius = 5.0 * scale;
                let spinner_thickness = 1.5 * scale;
                let spinner_cx = indicator_x + spinner_radius;
                let spinner_cy = bounds.y + bounds.height / 2.0;

                // Spinning arc: 270 degrees, 1 revolution per second
                let rotation = elapsed * std::f32::consts::TAU;
                let arc_span = std::f32::consts::TAU * 0.75; // 270 degrees
                let spinner_color = theme::ACCENT.to_array();

                output.spline_vertices.extend(create_arc_vertices(
                    spinner_cx, spinner_cy,
                    spinner_radius, spinner_thickness,
                    rotation, arc_span,
                    spinner_color,
                ));

                // Label text after spinner
                let label_x = indicator_x + spinner_radius * 2.0 + 6.0 * scale;
                output.bold_text_vertices.extend(bold_renderer.layout_text(
                    op_label, label_x, text_y,
                    theme::TEXT_MUTED.to_array(),
                ));
            }
        }

        // Button bounds
        let (fetch_bounds, pull_bounds, push_bounds, commit_bounds, help_bounds, settings_bounds) =
            self.button_bounds(bounds);

        // Async operation button rendering with pulsing background and spinning arc
        let async_buttons: [(&Button, Rect, bool); 3] = [
            (&self.fetch_button, fetch_bounds, self.fetching),
            (&self.pull_button, pull_bounds, self.pulling),
            (&self.push_button, push_bounds, self.pushing),
        ];

        for (button, btn_bounds, is_active) in &async_buttons {
            if *is_active {
                // Pulsing background: subtle glow effect
                let pulse = (elapsed * 3.0).sin() * 0.5 + 0.5; // 0..1 at ~0.5Hz
                let pulse_color = [
                    theme::SURFACE_RAISED.r + (theme::ACCENT.r - theme::SURFACE_RAISED.r) * pulse * 0.12,
                    theme::SURFACE_RAISED.g + (theme::ACCENT.g - theme::SURFACE_RAISED.g) * pulse * 0.12,
                    theme::SURFACE_RAISED.b + (theme::ACCENT.b - theme::SURFACE_RAISED.b) * pulse * 0.12,
                    1.0,
                ];
                let corner_radius = (btn_bounds.height * 0.20).min(8.0);
                output.spline_vertices.extend(create_rounded_rect_vertices(
                    btn_bounds,
                    pulse_color,
                    corner_radius,
                ));

                // Spinning arc indicator inside the button (left side)
                let spinner_radius = 5.0 * scale;
                let spinner_thickness = 1.5 * scale;
                let spinner_cx = btn_bounds.x + 10.0 * scale + spinner_radius;
                let spinner_cy = btn_bounds.y + btn_bounds.height / 2.0;

                let rotation = elapsed * std::f32::consts::TAU; // 1 rev/sec
                let arc_span = std::f32::consts::TAU * 0.75; // 270 degrees
                let spinner_color = theme::ACCENT.with_alpha(0.9).to_array();

                output.spline_vertices.extend(create_arc_vertices(
                    spinner_cx, spinner_cy,
                    spinner_radius, spinner_thickness,
                    rotation, arc_span,
                    spinner_color,
                ));

                // Render button text (shifted right to make room for spinner)
                let display_text = &button.label;
                let text_width = bold_renderer.measure_text(display_text);
                let text_area_x = spinner_cx + spinner_radius + 4.0 * scale;
                let text_area_w = btn_bounds.right() - text_area_x;
                let text_x = text_area_x + (text_area_w - text_width) / 2.0;
                let btn_text_y = btn_bounds.y + (btn_bounds.height - line_height) / 2.0;

                output.bold_text_vertices.extend(bold_renderer.layout_text(
                    display_text,
                    text_x,
                    btn_text_y,
                    theme::TEXT_BRIGHT.to_array(),
                ));
            } else {
                output.extend(button.layout_with_bold(text_renderer, bold_renderer, *btn_bounds));
            }
        }

        // Commit button (no async state)
        output.extend(self.commit_button.layout_with_bold(text_renderer, bold_renderer, commit_bounds));

        // Help and Settings buttons (ghost style - keep regular weight)
        output.extend(self.help_button.layout(text_renderer, help_bounds));
        output.extend(self.settings_button.layout(text_renderer, settings_bounds));

        // Drop shadow below header
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

impl Widget for HeaderBar {
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

        // Handle abort button (when operation in progress)
        if let Some(abort_bounds) = self.abort_button_bounds {
            if self.abort_button.handle_event(event, abort_bounds).is_consumed() {
                if self.abort_button.was_clicked() {
                    self.pending_action = Some(HeaderAction::AbortOperation);
                }
                return EventResponse::Consumed;
            }
        }

        // Handle button events
        if self.fetch_button.handle_event(event, fetch_bounds).is_consumed() {
            if self.fetch_button.was_clicked() {
                self.pending_action = Some(HeaderAction::Fetch);
            }
            return EventResponse::Consumed;
        }

        // Track shift state for pull --rebase detection
        if let InputEvent::MouseDown { modifiers, .. } | InputEvent::MouseUp { modifiers, .. } = event {
            if pull_bounds.contains(event.position().unwrap_or((0.0, 0.0)).0, event.position().unwrap_or((0.0, 0.0)).1) {
                self.pull_shift_held = modifiers.shift;
            }
        }
        if self.pull_button.handle_event(event, pull_bounds).is_consumed() {
            if self.pull_button.was_clicked() {
                if self.pull_shift_held {
                    self.pending_action = Some(HeaderAction::PullRebase);
                } else {
                    self.pending_action = Some(HeaderAction::Pull);
                }
                self.pull_shift_held = false;
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

        // Abort button hover
        if let Some(abort_bounds) = self.abort_button_bounds {
            self.abort_button.update_hover(x, y, abort_bounds);
        }

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
            // Skip repo name when it matches the branch (avoids "main | main" redundancy)
            if self.repo_name == self.branch_name {
                branch_pill_x = bounds.x + 16.0;
            } else {
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
            }
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

        // Remote name indicator (e.g. "origin") in muted text after branch pill
        let mut after_pill_x = branch_pill_x + pill_w;
        if !self.remote_name.is_empty() {
            let remote_label = format!("{}", self.remote_name);
            let remote_x = after_pill_x + 8.0;
            output.text_vertices.extend(text_renderer.layout_text(
                &remote_label,
                remote_x,
                text_y,
                theme::TEXT_MUTED.to_array(),
            ));
            after_pill_x = remote_x + text_renderer.measure_text(&remote_label);
        }

        // Operation state banner (e.g. "MERGE IN PROGRESS" + Abort button)
        if let Some(label) = self.operation_state_label {
            // Amber warning text after the branch pill
            let label_x = after_pill_x + 12.0;
            let label_color = [1.0, 0.718, 0.302, 1.0]; // amber #FFB74D
            output.text_vertices.extend(text_renderer.layout_text(
                label,
                label_x,
                text_y,
                label_color,
            ));

            // Abort button (pre-computed bounds)
            if let Some(abort_bounds) = self.abort_button_bounds {
                output.extend(self.abort_button.layout(text_renderer, abort_bounds));
            }
        }

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
