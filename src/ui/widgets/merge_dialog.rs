//! Merge strategy dialog - modal overlay for choosing merge strategy

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{
    create_dialog_backdrop, create_rect_vertices, theme, Widget, WidgetOutput,
};
use crate::ui::widgets::{Button, TextInput};
use crate::ui::{Color, Rect, TextRenderer};

/// The merge strategy to use
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeStrategy {
    /// Default: fast-forward if possible, merge commit otherwise
    Default,
    /// Always create a merge commit (--no-ff)
    NoFastForward,
    /// Only fast-forward, fail if not possible (--ff-only)
    FastForwardOnly,
    /// Squash all commits into staged changes (--squash)
    Squash,
}

/// Actions from the merge dialog
#[derive(Clone, Debug)]
pub enum MergeDialogAction {
    /// Confirm merge with (branch, strategy, optional commit message)
    Confirm(String, MergeStrategy, Option<String>),
    /// User cancelled
    Cancel,
}

/// A modal dialog for choosing merge strategy
pub struct MergeDialog {
    visible: bool,
    /// The branch being merged
    branch_name: String,
    /// The current branch (target)
    current_branch: String,
    /// Selected merge strategy
    strategy: MergeStrategy,
    /// Commit message input (for --no-ff and --squash)
    message_input: TextInput,
    /// Confirm (Merge) button
    merge_button: Button,
    /// Cancel button
    cancel_button: Button,
    /// Pending action to be consumed
    pending_action: Option<MergeDialogAction>,
    /// Warning text to display (e.g. dirty workdir)
    warning: Option<String>,
    /// Number of uncommitted changes (for warning display)
    uncommitted_count: usize,
}

impl MergeDialog {
    pub fn new() -> Self {
        Self {
            visible: false,
            branch_name: String::new(),
            current_branch: String::new(),
            strategy: MergeStrategy::Default,
            message_input: TextInput::new().with_placeholder("Merge commit message..."),
            merge_button: Button::new("Merge").primary(),
            cancel_button: Button::new("Cancel"),
            pending_action: None,
            warning: None,
            uncommitted_count: 0,
        }
    }

    /// Show the dialog for merging a branch.
    /// `uncommitted` is the number of uncommitted changes (for warning display).
    pub fn show(&mut self, branch_name: &str, current_branch: &str, uncommitted: usize) {
        self.visible = true;
        self.branch_name = branch_name.to_string();
        self.current_branch = current_branch.to_string();
        self.strategy = MergeStrategy::Default;
        self.uncommitted_count = uncommitted;

        // Pre-fill commit message with git default
        let default_msg = format!("Merge branch '{}'", branch_name);
        self.message_input = TextInput::new().with_placeholder("Merge commit message...");
        self.message_input.set_text(&default_msg);
        self.message_input.set_focused(false);

        self.merge_button = Button::new("Merge").primary();
        self.pending_action = None;

        // Set warning if there are uncommitted changes
        if uncommitted > 0 {
            self.warning = Some(format!(
                "Warning: {} uncommitted change{}. Stash or commit first.",
                uncommitted,
                if uncommitted == 1 { "" } else { "s" },
            ));
        } else {
            self.warning = None;
        }
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.message_input.set_focused(false);
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn take_action(&mut self) -> Option<MergeDialogAction> {
        self.pending_action.take()
    }

    /// Whether the commit message input should be shown
    fn show_message_input(&self) -> bool {
        matches!(self.strategy, MergeStrategy::NoFastForward | MergeStrategy::Squash)
    }

    fn try_confirm(&mut self) {
        let message = if self.show_message_input() {
            let msg = self.message_input.text().trim().to_string();
            if msg.is_empty() && self.strategy == MergeStrategy::NoFastForward {
                // --no-ff requires a message
                return;
            }
            Some(msg)
        } else {
            None
        };
        self.pending_action = Some(MergeDialogAction::Confirm(
            self.branch_name.clone(),
            self.strategy,
            message,
        ));
        self.hide();
    }

    /// Compute dialog bounds centered in screen
    fn dialog_bounds(&self, screen: Rect, scale: f32) -> Rect {
        let dialog_w = (420.0 * scale).min(screen.width * 0.85);
        let base_h = 220.0;
        let extra = if self.show_message_input() { 60.0 } else { 0.0 };
        let warning_h = if self.warning.is_some() { 24.0 } else { 0.0 };
        let dialog_h = ((base_h + extra + warning_h) * scale).min(screen.height * 0.8);
        let dialog_x = screen.x + (screen.width - dialog_w) / 2.0;
        let dialog_y = screen.y + (screen.height - dialog_h) / 2.0;
        Rect::new(dialog_x, dialog_y, dialog_w, dialog_h)
    }

    /// Radio button bounds for strategy selection
    fn strategy_items() -> &'static [(MergeStrategy, &'static str)] {
        &[
            (MergeStrategy::Default, "Default (fast-forward if possible)"),
            (MergeStrategy::NoFastForward, "Create merge commit (--no-ff)"),
            (MergeStrategy::FastForwardOnly, "Fast-forward only (--ff-only)"),
            (MergeStrategy::Squash, "Squash commits (--squash)"),
        ]
    }
}

impl Widget for MergeDialog {
    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        if !self.visible {
            return EventResponse::Ignored;
        }

        let scale = (bounds.height / 720.0).max(1.0);
        let dialog = self.dialog_bounds(bounds, scale);
        let padding = 16.0 * scale;
        let line_h = 32.0 * scale;
        let radio_line_h = 22.0 * scale;

        // Handle keyboard shortcuts first
        if let InputEvent::KeyDown { key, .. } = event {
            match key {
                Key::Escape => {
                    self.pending_action = Some(MergeDialogAction::Cancel);
                    self.hide();
                    return EventResponse::Consumed;
                }
                Key::Enter => {
                    if !self.message_input.is_focused() {
                        self.try_confirm();
                        return EventResponse::Consumed;
                    }
                }
                _ => {}
            }
        }

        // Radio button click detection
        if let InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } = event {
            let radio_start_y = dialog.y + 60.0 * scale;
            let items = Self::strategy_items();
            for (i, (strategy, _label)) in items.iter().enumerate() {
                let item_y = radio_start_y + i as f32 * radio_line_h;
                let radio_bounds = Rect::new(
                    dialog.x + padding,
                    item_y,
                    dialog.width - padding * 2.0,
                    radio_line_h,
                );
                if radio_bounds.contains(*x, *y) {
                    self.strategy = *strategy;
                    // Update message input visibility/text based on strategy
                    if self.strategy == MergeStrategy::NoFastForward {
                        let msg = format!("Merge branch '{}'", self.branch_name);
                        self.message_input.set_text(&msg);
                    } else if self.strategy == MergeStrategy::Squash {
                        self.message_input.set_text("");
                    }
                    return EventResponse::Consumed;
                }
            }
        }

        // Message input handling (when visible)
        if self.show_message_input() {
            let radio_end_y = dialog.y + 60.0 * scale + 4.0 * 22.0 * scale;
            let label_h = 18.0 * scale;
            let input_y = radio_end_y + label_h + 4.0 * scale;
            let input_w = dialog.width - padding * 2.0;
            let input_bounds = Rect::new(dialog.x + padding, input_y, input_w, line_h);

            // Click-to-focus
            if let InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } = event {
                if input_bounds.contains(*x, *y) {
                    self.message_input.set_focused(true);
                } else {
                    self.message_input.set_focused(false);
                }
            }

            if self.message_input.handle_event(event, input_bounds).is_consumed() {
                return EventResponse::Consumed;
            }
        }

        // Button bounds
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let button_gap = 8.0 * scale;
        let cancel_x = dialog.right() - padding - button_w;
        let merge_x = cancel_x - button_w - button_gap;
        let merge_bounds = Rect::new(merge_x, button_y, button_w, line_h);
        let cancel_bounds = Rect::new(cancel_x, button_y, button_w, line_h);

        if self.merge_button.handle_event(event, merge_bounds).is_consumed() {
            if self.merge_button.was_clicked() {
                self.try_confirm();
            }
            return EventResponse::Consumed;
        }

        if self.cancel_button.handle_event(event, cancel_bounds).is_consumed() {
            if self.cancel_button.was_clicked() {
                self.pending_action = Some(MergeDialogAction::Cancel);
                self.hide();
            }
            return EventResponse::Consumed;
        }

        // Click outside dialog dismisses (cancel)
        if let InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } = event
            && !dialog.contains(*x, *y)
        {
            self.pending_action = Some(MergeDialogAction::Cancel);
            self.hide();
            return EventResponse::Consumed;
        }

        // Consume all events while dialog is visible (modal)
        EventResponse::Consumed
    }

    fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        self.layout_with_bold(text_renderer, text_renderer, bounds)
    }
}

impl MergeDialog {
    pub fn layout_with_bold(&self, text_renderer: &TextRenderer, bold_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        if !self.visible {
            return output;
        }

        let scale = (bounds.height / 720.0).max(1.0);
        let dialog = self.dialog_bounds(bounds, scale);
        let padding = 16.0 * scale;
        let line_h = 32.0 * scale;
        let radio_line_h = 22.0 * scale;

        // Backdrop + shadow + dialog background
        create_dialog_backdrop(&mut output, &bounds, &dialog, scale);

        // Title (bold)
        let title = format!("Merge '{}' into {}", self.branch_name, self.current_branch);
        let title_y = dialog.y + padding;
        output.bold_text_vertices.extend(bold_renderer.layout_text(
            &title,
            dialog.x + padding,
            title_y,
            theme::TEXT_BRIGHT.to_array(),
        ));

        // Title separator
        let sep_y = dialog.y + 36.0 * scale;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(dialog.x + padding, sep_y, dialog.width - padding * 2.0, 1.0),
            theme::BORDER.with_alpha(0.4).to_array(),
        ));

        // Strategy label
        let strategy_label_y = dialog.y + 44.0 * scale;
        output.text_vertices.extend(text_renderer.layout_text(
            "Strategy:",
            dialog.x + padding,
            strategy_label_y,
            theme::TEXT_MUTED.to_array(),
        ));

        // Radio buttons
        let radio_start_y = dialog.y + 60.0 * scale;
        let items = Self::strategy_items();
        let radio_r = 5.0 * scale;
        let radio_cx = dialog.x + padding + radio_r + 2.0 * scale;

        for (i, (strategy, label)) in items.iter().enumerate() {
            let item_y = radio_start_y + i as f32 * radio_line_h;
            let cy = item_y + radio_line_h / 2.0;
            let selected = self.strategy == *strategy;

            // Draw radio circle (outer ring)
            let ring_color = if selected {
                Color::rgba(0.259, 0.647, 0.961, 1.0) // #42A5F5 blue
            } else {
                theme::TEXT_MUTED
            };
            let segments = 12;
            for seg in 0..segments {
                let angle0 = 2.0 * std::f32::consts::PI * (seg as f32 / segments as f32);
                let angle1 = 2.0 * std::f32::consts::PI * ((seg + 1) as f32 / segments as f32);
                let outer_r = radio_r;
                let inner_r = radio_r - 1.5 * scale;

                // Outer edge triangle pair forming ring segment
                let cos0 = angle0.cos();
                let sin0 = angle0.sin();
                let cos1 = angle1.cos();
                let sin1 = angle1.sin();

                output.spline_vertices.push(crate::ui::SplineVertex {
                    position: [radio_cx + cos0 * inner_r, cy + sin0 * inner_r],
                    color: ring_color.to_array(),
                });
                output.spline_vertices.push(crate::ui::SplineVertex {
                    position: [radio_cx + cos0 * outer_r, cy + sin0 * outer_r],
                    color: ring_color.to_array(),
                });
                output.spline_vertices.push(crate::ui::SplineVertex {
                    position: [radio_cx + cos1 * outer_r, cy + sin1 * outer_r],
                    color: ring_color.to_array(),
                });

                output.spline_vertices.push(crate::ui::SplineVertex {
                    position: [radio_cx + cos0 * inner_r, cy + sin0 * inner_r],
                    color: ring_color.to_array(),
                });
                output.spline_vertices.push(crate::ui::SplineVertex {
                    position: [radio_cx + cos1 * outer_r, cy + sin1 * outer_r],
                    color: ring_color.to_array(),
                });
                output.spline_vertices.push(crate::ui::SplineVertex {
                    position: [radio_cx + cos1 * inner_r, cy + sin1 * inner_r],
                    color: ring_color.to_array(),
                });
            }

            // Draw filled center for selected radio
            if selected {
                let fill_r = radio_r * 0.5;
                for seg in 0..segments {
                    let angle0 = 2.0 * std::f32::consts::PI * (seg as f32 / segments as f32);
                    let angle1 = 2.0 * std::f32::consts::PI * ((seg + 1) as f32 / segments as f32);
                    output.spline_vertices.push(crate::ui::SplineVertex {
                        position: [radio_cx, cy],
                        color: ring_color.to_array(),
                    });
                    output.spline_vertices.push(crate::ui::SplineVertex {
                        position: [radio_cx + angle0.cos() * fill_r, cy + angle0.sin() * fill_r],
                        color: ring_color.to_array(),
                    });
                    output.spline_vertices.push(crate::ui::SplineVertex {
                        position: [radio_cx + angle1.cos() * fill_r, cy + angle1.sin() * fill_r],
                        color: ring_color.to_array(),
                    });
                }
            }

            // Label text
            let text_x = radio_cx + radio_r + 8.0 * scale;
            let text_color = if selected { theme::TEXT_BRIGHT } else { theme::TEXT };
            output.text_vertices.extend(text_renderer.layout_text(
                label,
                text_x,
                item_y + 3.0 * scale,
                text_color.to_array(),
            ));
        }

        // Commit message input (conditional)
        if self.show_message_input() {
            let radio_end_y = radio_start_y + items.len() as f32 * radio_line_h;
            let label_y = radio_end_y + 2.0 * scale;
            output.text_vertices.extend(text_renderer.layout_text(
                "Commit message:",
                dialog.x + padding,
                label_y,
                theme::TEXT_MUTED.to_array(),
            ));

            let label_h = 18.0 * scale;
            let input_y = radio_end_y + label_h + 4.0 * scale;
            let input_w = dialog.width - padding * 2.0;
            let input_bounds = Rect::new(dialog.x + padding, input_y, input_w, line_h);
            output.extend(self.message_input.layout(text_renderer, input_bounds));
        }

        // Warning text (amber)
        if let Some(ref warning) = self.warning {
            let warning_y = dialog.bottom() - padding - line_h - 20.0 * scale;
            let amber = [1.0, 0.718, 0.302, 1.0]; // #FFB74D
            output.text_vertices.extend(text_renderer.layout_text(
                warning,
                dialog.x + padding,
                warning_y,
                amber,
            ));
        }

        // Buttons at bottom
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let button_gap = 8.0 * scale;
        let cancel_x = dialog.right() - padding - button_w;
        let merge_x = cancel_x - button_w - button_gap;

        // Button separator
        let btn_sep_y = button_y - 8.0 * scale;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(dialog.x + padding, btn_sep_y, dialog.width - padding * 2.0, 1.0),
            theme::BORDER.with_alpha(0.4).to_array(),
        ));

        let merge_bounds = Rect::new(merge_x, button_y, button_w, line_h);
        let cancel_bounds = Rect::new(cancel_x, button_y, button_w, line_h);

        output.extend(self.merge_button.layout(text_renderer, merge_bounds));
        output.extend(self.cancel_button.layout(text_renderer, cancel_bounds));

        output
    }
}
