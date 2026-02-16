//! Repository open dialog - modal overlay for entering a repo path

use std::path::PathBuf;
use std::sync::mpsc;

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{
    create_dialog_backdrop, create_rect_vertices, create_rounded_rect_vertices, theme, Widget,
    WidgetOutput,
};
use crate::ui::widgets::{Button, TextInput};
use crate::ui::{Rect, TextRenderer};

/// Actions from the repo dialog
#[derive(Clone, Debug)]
pub enum RepoDialogAction {
    Open(PathBuf),
    Cancel,
}

/// A modal dialog for opening a repository by path
pub struct RepoDialog {
    visible: bool,
    path_input: TextInput,
    browse_button: Button,
    open_button: Button,
    cancel_button: Button,
    error_message: Option<String>,
    pending_action: Option<RepoDialogAction>,
    /// Channel receiver for async native file picker results
    picker_rx: Option<mpsc::Receiver<String>>,
    /// Recent repo paths to show for quick re-opening
    recent_repos: Vec<String>,
    /// Which recent repo item is hovered (-1 = none)
    hovered_recent: i32,
}

impl RepoDialog {
    pub fn new() -> Self {
        Self {
            visible: false,
            path_input: TextInput::new().with_placeholder("/path/to/repository"),
            browse_button: Button::new("Browse..."),
            open_button: Button::new("Open").primary(),
            cancel_button: Button::new("Cancel"),
            error_message: None,
            pending_action: None,
            picker_rx: None,
            recent_repos: Vec::new(),
            hovered_recent: -1,
        }
    }

    pub fn show(&mut self) {
        self.visible = true;
        self.path_input.set_text("");
        self.path_input.set_focused(true);
        self.error_message = None;
        self.hovered_recent = -1;
    }

    /// Show the dialog with recent repos populated from config.
    pub fn show_with_recent(&mut self, recent_repos: &[String]) {
        self.show();
        self.recent_repos = recent_repos.to_vec();
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.path_input.set_focused(false);
        self.error_message = None;
        self.hovered_recent = -1;
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn take_action(&mut self) -> Option<RepoDialogAction> {
        self.pending_action.take()
    }

    /// Poll the file picker channel for results. Call once per frame.
    pub fn poll_picker(&mut self) {
        if let Some(ref rx) = self.picker_rx {
            match rx.try_recv() {
                Ok(path) => {
                    self.path_input.set_text(&path);
                    self.error_message = None;
                    self.picker_rx = None;
                    // Auto-open: validate and open immediately
                    self.try_open();
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    // Dialog was cancelled or thread finished without sending
                    self.picker_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    // Still waiting
                }
            }
        }
    }

    fn open_native_picker(&mut self) {
        if self.picker_rx.is_some() {
            return; // Already waiting for a picker result
        }
        let (tx, rx) = mpsc::channel();
        let start_dir = if self.path_input.text().trim().is_empty() {
            std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
        } else {
            self.path_input.text().trim().to_string()
        };
        std::thread::spawn(move || {
            if let Some(folder) = rfd::FileDialog::new()
                .set_directory(&start_dir)
                .set_title("Open Git Repository")
                .pick_folder()
            {
                let _ = tx.send(folder.to_string_lossy().to_string());
            }
        });
        self.picker_rx = Some(rx);
    }

    fn try_open(&mut self) {
        let path_str = self.path_input.text().trim().to_string();
        if path_str.is_empty() {
            self.error_message = Some("Please enter a path".to_string());
            return;
        }

        let path = PathBuf::from(&path_str);

        // Validate it's a git repo by trying to discover
        match git2::Repository::discover(&path) {
            Ok(_) => {
                self.pending_action = Some(RepoDialogAction::Open(path));
                self.hide();
            }
            Err(e) => {
                self.error_message = Some(format!("Not a git repository: {}", e));
            }
        }
    }

    fn try_open_recent(&mut self, index: usize) {
        if let Some(path_str) = self.recent_repos.get(index).cloned() {
            self.path_input.set_text(&path_str);
            self.try_open();
        }
    }

    /// Compute dialog bounds centered in screen.
    /// Height scales with the number of recent repos shown.
    fn dialog_bounds(&self, screen: Rect, scale: f32) -> Rect {
        let recent_count = self.recent_repos.len().min(5) as f32;
        let recent_section_h = if recent_count > 0.0 {
            // Section label + items + gap
            (20.0 + recent_count * 26.0 + 8.0) * scale
        } else {
            0.0
        };
        let dialog_w = (440.0 * scale).min(screen.width * 0.85);
        let dialog_h = ((200.0 * scale) + recent_section_h).min(screen.height * 0.7);
        let dialog_x = screen.x + (screen.width - dialog_w) / 2.0;
        let dialog_y = screen.y + (screen.height - dialog_h) / 2.0;
        Rect::new(dialog_x, dialog_y, dialog_w, dialog_h)
    }

    /// Compute bounds for each recent repo item (for hit testing + layout).
    fn recent_item_bounds(&self, dialog: Rect, scale: f32) -> Vec<Rect> {
        let padding = 16.0 * scale;
        let line_h = 32.0 * scale;
        let input_y = dialog.y + 44.0 * scale;
        let recent_start_y = input_y + line_h + 8.0 * scale + 20.0 * scale;
        let item_h = 26.0 * scale;
        let count = self.recent_repos.len().min(5);
        (0..count)
            .map(|i| {
                Rect::new(
                    dialog.x + padding,
                    recent_start_y + i as f32 * item_h,
                    dialog.width - padding * 2.0,
                    item_h,
                )
            })
            .collect()
    }
}

impl Widget for RepoDialog {
    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        if !self.visible {
            return EventResponse::Ignored;
        }

        let scale = (bounds.height / 720.0).max(1.0);
        let dialog = self.dialog_bounds(bounds, scale);
        let padding = 16.0 * scale;
        let line_h = 32.0 * scale;

        // Input field bounds (narrower to make room for Browse button)
        let browse_w = 90.0 * scale;
        let browse_gap = 8.0 * scale;
        let input_y = dialog.y + 44.0 * scale;
        let input_bounds = Rect::new(
            dialog.x + padding,
            input_y,
            dialog.width - padding * 2.0 - browse_w - browse_gap,
            line_h,
        );

        // Browse button bounds (right of input)
        let browse_bounds = Rect::new(
            input_bounds.right() + browse_gap,
            input_y,
            browse_w,
            line_h,
        );

        // Button bounds at bottom
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let button_gap = 8.0 * scale;
        let cancel_x = dialog.right() - padding - button_w;
        let open_x = cancel_x - button_w - button_gap;
        let open_bounds = Rect::new(open_x, button_y, button_w, line_h);
        let cancel_bounds = Rect::new(cancel_x, button_y, button_w, line_h);

        // Handle keyboard shortcuts
        if let InputEvent::KeyDown { key, .. } = event {
            match key {
                Key::Escape => {
                    self.pending_action = Some(RepoDialogAction::Cancel);
                    self.hide();
                    return EventResponse::Consumed;
                }
                Key::Enter => {
                    self.try_open();
                    return EventResponse::Consumed;
                }
                _ => {}
            }
        }

        // Handle mouse move for recent item hover
        if let InputEvent::MouseMove { x, y, .. } = event {
            let recent_bounds = self.recent_item_bounds(dialog, scale);
            self.hovered_recent = -1;
            for (i, rb) in recent_bounds.iter().enumerate() {
                if rb.contains(*x, *y) {
                    self.hovered_recent = i as i32;
                    break;
                }
            }
        }

        // Handle click on recent items
        if let InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } = event {
            let recent_bounds = self.recent_item_bounds(dialog, scale);
            for (i, rb) in recent_bounds.iter().enumerate() {
                if rb.contains(*x, *y) {
                    self.try_open_recent(i);
                    return EventResponse::Consumed;
                }
            }
        }

        // Route to text input
        if self.path_input.handle_event(event, input_bounds).is_consumed() {
            self.error_message = None; // Clear error on edit
            return EventResponse::Consumed;
        }

        // Route to browse button
        if self.browse_button.handle_event(event, browse_bounds).is_consumed() {
            if self.browse_button.was_clicked() {
                self.open_native_picker();
            }
            return EventResponse::Consumed;
        }

        // Route to action buttons
        if self.open_button.handle_event(event, open_bounds).is_consumed() {
            if self.open_button.was_clicked() {
                self.try_open();
            }
            return EventResponse::Consumed;
        }

        if self.cancel_button.handle_event(event, cancel_bounds).is_consumed() {
            if self.cancel_button.was_clicked() {
                self.pending_action = Some(RepoDialogAction::Cancel);
                self.hide();
            }
            return EventResponse::Consumed;
        }

        // Click outside dialog dismisses
        if let InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } = event
            && !dialog.contains(*x, *y) {
                self.pending_action = Some(RepoDialogAction::Cancel);
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

impl RepoDialog {
    pub fn layout_with_bold(&self, text_renderer: &TextRenderer, bold_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        if !self.visible {
            return output;
        }

        let scale = (bounds.height / 720.0).max(1.0);
        let dialog = self.dialog_bounds(bounds, scale);
        let padding = 16.0 * scale;
        let line_h = 32.0 * scale;
        let line_height = text_renderer.line_height();

        // Backdrop + shadow + dialog background
        create_dialog_backdrop(&mut output, &bounds, &dialog, scale);

        // Title (bold)
        let title_y = dialog.y + padding;
        output.bold_text_vertices.extend(bold_renderer.layout_text(
            "Open Repository",
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

        // Input field (narrower to accommodate Browse button)
        let browse_w = 90.0 * scale;
        let browse_gap = 8.0 * scale;
        let input_y = dialog.y + 44.0 * scale;
        let input_bounds = Rect::new(
            dialog.x + padding,
            input_y,
            dialog.width - padding * 2.0 - browse_w - browse_gap,
            line_h,
        );
        output.extend(self.path_input.layout(text_renderer, input_bounds));

        // Browse button
        let browse_bounds = Rect::new(
            input_bounds.right() + browse_gap,
            input_y,
            browse_w,
            line_h,
        );
        output.extend(self.browse_button.layout(text_renderer, browse_bounds));

        // Error message (below input)
        if let Some(ref err) = self.error_message {
            let err_y = input_y + line_h + 4.0 * scale;
            output.text_vertices.extend(text_renderer.layout_text(
                err,
                dialog.x + padding,
                err_y,
                theme::STATUS_DIRTY.to_array(),
            ));
        }

        // Waiting indicator for native picker
        if self.picker_rx.is_some() {
            let wait_y = input_y + line_h + 4.0 * scale;
            output.text_vertices.extend(text_renderer.layout_text(
                "Waiting for folder selection...",
                dialog.x + padding,
                wait_y,
                theme::TEXT_MUTED.to_array(),
            ));
        }

        // Recent repos section
        if !self.recent_repos.is_empty() {
            let recent_label_y = input_y + line_h + 8.0 * scale;
            output.text_vertices.extend(text_renderer.layout_text(
                "Recent",
                dialog.x + padding,
                recent_label_y,
                theme::TEXT_MUTED.to_array(),
            ));

            let item_h = 26.0 * scale;
            let recent_start_y = recent_label_y + 20.0 * scale;
            let item_corner = 4.0 * scale;

            for (i, path) in self.recent_repos.iter().take(5).enumerate() {
                let item_y = recent_start_y + i as f32 * item_h;
                let item_rect = Rect::new(
                    dialog.x + padding,
                    item_y,
                    dialog.width - padding * 2.0,
                    item_h,
                );

                // Hover highlight
                if self.hovered_recent == i as i32 {
                    output.spline_vertices.extend(create_rounded_rect_vertices(
                        &item_rect,
                        [1.0, 1.0, 1.0, 0.06],
                        item_corner,
                    ));
                }

                // Truncate path display if too long
                let display_path = if path.len() > 50 {
                    format!("...{}", &path[path.len() - 47..])
                } else {
                    path.clone()
                };

                let text_y = item_y + (item_h - line_height) / 2.0;
                let color = if self.hovered_recent == i as i32 {
                    theme::TEXT_BRIGHT
                } else {
                    theme::TEXT
                };
                output.text_vertices.extend(text_renderer.layout_text(
                    &display_path,
                    dialog.x + padding + 4.0 * scale,
                    text_y,
                    color.to_array(),
                ));
            }
        }

        // Buttons at bottom
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let button_gap = 8.0 * scale;
        let cancel_x = dialog.right() - padding - button_w;
        let open_x = cancel_x - button_w - button_gap;

        // Button separator
        let btn_sep_y = button_y - 8.0 * scale;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(dialog.x + padding, btn_sep_y, dialog.width - padding * 2.0, 1.0),
            theme::BORDER.with_alpha(0.4).to_array(),
        ));

        let open_bounds = Rect::new(open_x, button_y, button_w, line_h);
        let cancel_bounds = Rect::new(cancel_x, button_y, button_w, line_h);

        output.extend(self.open_button.layout(text_renderer, open_bounds));
        output.extend(self.cancel_button.layout(text_renderer, cancel_bounds));

        // Hint text
        let hint_y = button_y + (line_h - line_height) / 2.0;
        output.text_vertices.extend(text_renderer.layout_text(
            "Enter to open  |  Ctrl+O Browse",
            dialog.x + padding,
            hint_y,
            theme::TEXT_MUTED.to_array(),
        ));

        output
    }
}
