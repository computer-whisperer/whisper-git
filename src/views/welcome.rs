//! Welcome view — fills the main area when no repo is open.
//!
//! Two-column layout: a left action sidebar (Open / Clone / Init + a path
//! input) and a right column listing recent repos. Returns a `WelcomeAction`
//! for the App to dispatch (open a repo, show the clone dialog, init then
//! open).

use std::path::PathBuf;
use std::sync::mpsc;
use winit::event_loop::EventLoopProxy;

use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::text_util::truncate_to_width;
use crate::ui::widget::{
    Widget, WidgetOutput, create_rect_vertices, create_rounded_rect_vertices, theme,
};
use crate::ui::widgets::{Button, TextInput};
use crate::ui::{Rect, TextRenderer};

#[derive(Clone, Debug)]
pub enum WelcomeAction {
    Open(PathBuf),
    Clone,
    InitAt(PathBuf),
}

struct RecentEntry {
    path: PathBuf,
    name: String,
    parent: String,
}

impl RecentEntry {
    fn from_path_str(s: &str) -> Self {
        let path = PathBuf::from(s);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| s.to_string());
        let parent = path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        Self { path, name, parent }
    }
}

pub struct WelcomeView {
    recent: Vec<RecentEntry>,
    open_button: Button,
    clone_button: Button,
    init_button: Button,
    path_input: TextInput,
    open_path_button: Button,
    /// Index of currently-hovered recent item, -1 = none.
    hovered_recent: i32,
    open_picker_rx: Option<mpsc::Receiver<String>>,
    init_picker_rx: Option<mpsc::Receiver<String>>,
    pending_action: Option<WelcomeAction>,
    proxy: Option<EventLoopProxy<()>>,
}

impl Default for WelcomeView {
    fn default() -> Self {
        Self::new()
    }
}

impl WelcomeView {
    pub fn new() -> Self {
        Self {
            recent: Vec::new(),
            open_button: Button::new("Open Local…").primary(),
            clone_button: Button::new("Clone Remote…"),
            init_button: Button::new("Init New…"),
            path_input: TextInput::new().with_placeholder("/path/to/repository"),
            open_path_button: Button::new("Open"),
            hovered_recent: -1,
            open_picker_rx: None,
            init_picker_rx: None,
            pending_action: None,
            proxy: None,
        }
    }

    pub fn set_proxy(&mut self, proxy: EventLoopProxy<()>) {
        self.proxy = Some(proxy);
    }

    /// Refresh the recent list from config. Called on show and after each open
    /// so MRU ordering stays current.
    pub fn set_recent(&mut self, recent_repos: &[String]) {
        self.recent = recent_repos
            .iter()
            .map(|s| RecentEntry::from_path_str(s))
            .collect();
    }

    pub fn take_action(&mut self) -> Option<WelcomeAction> {
        self.pending_action.take()
    }

    /// Drain any picker results queued from the file-dialog threads.
    pub fn poll_pickers(&mut self) {
        if let Some(ref rx) = self.open_picker_rx {
            match rx.try_recv() {
                Ok(p) => {
                    self.open_picker_rx = None;
                    self.pending_action = Some(WelcomeAction::Open(PathBuf::from(p)));
                }
                Err(mpsc::TryRecvError::Disconnected) => self.open_picker_rx = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(ref rx) = self.init_picker_rx {
            match rx.try_recv() {
                Ok(p) => {
                    self.init_picker_rx = None;
                    self.pending_action = Some(WelcomeAction::InitAt(PathBuf::from(p)));
                }
                Err(mpsc::TryRecvError::Disconnected) => self.init_picker_rx = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    fn spawn_picker(&mut self, title: &'static str, into_open: bool) {
        let pending = if into_open {
            self.open_picker_rx.is_some()
        } else {
            self.init_picker_rx.is_some()
        };
        if pending {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let proxy = self.proxy.clone();
        let start_dir = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        std::thread::spawn(move || {
            if let Some(folder) = rfd::FileDialog::new()
                .set_directory(&start_dir)
                .set_title(title)
                .pick_folder()
            {
                let _ = tx.send(folder.to_string_lossy().to_string());
            }
            if let Some(p) = proxy {
                let _ = p.send_event(());
            }
        });
        if into_open {
            self.open_picker_rx = Some(rx);
        } else {
            self.init_picker_rx = Some(rx);
        }
    }

    fn submit_path_input(&mut self) {
        let p = self.path_input.text().trim().to_string();
        if p.is_empty() {
            return;
        }
        self.path_input.set_text("");
        self.pending_action = Some(WelcomeAction::Open(PathBuf::from(p)));
    }

    /// Layout helper: split the bounds into (left action column, right recent column)
    /// with a fixed-width left column.
    fn split_columns(bounds: Rect) -> (Rect, Rect) {
        let left_w = 340.0_f32.min(bounds.width * 0.4);
        let left = Rect::new(bounds.x, bounds.y, left_w, bounds.height);
        let right = Rect::new(
            bounds.x + left_w,
            bounds.y,
            bounds.width - left_w,
            bounds.height,
        );
        (left, right)
    }

    /// Bounds of each control in the left action column.
    fn action_layout(left: Rect) -> ActionLayout {
        let pad = 28.0;
        let btn_h = 40.0;
        let gap = 12.0;
        let inner_x = left.x + pad;
        let inner_w = left.width - pad * 2.0;
        let title_y = left.y + pad + 8.0;
        let buttons_y = title_y + 64.0;
        let open = Rect::new(inner_x, buttons_y, inner_w, btn_h);
        let clone = Rect::new(inner_x, open.bottom() + gap, inner_w, btn_h);
        let init = Rect::new(inner_x, clone.bottom() + gap, inner_w, btn_h);
        let path_label_y = init.bottom() + 36.0;
        let path_input_y = path_label_y + 24.0;
        // Path input + Open stacked vertically — a side-by-side layout doesn't
        // leave enough room in the narrow action column for the placeholder.
        let path_input = Rect::new(inner_x, path_input_y, inner_w, 34.0);
        let path_open_btn = Rect::new(
            inner_x,
            path_input.bottom() + 8.0,
            inner_w,
            path_input.height,
        );
        ActionLayout {
            title_y,
            open,
            clone,
            init,
            path_label_y,
            path_input,
            path_open_btn,
        }
    }

    /// Bounds of recent-list items.
    fn recent_item_bounds(right: Rect) -> Vec<Rect> {
        let pad_x = 28.0;
        let pad_top = 64.0;
        let item_h = 60.0;
        let gap = 8.0;
        let inner_x = right.x + pad_x;
        let inner_w = right.width - pad_x * 2.0;
        let max_items = ((right.height - pad_top - 20.0) / (item_h + gap))
            .floor()
            .max(0.0) as usize;
        let count = max_items;
        (0..count)
            .map(|i| {
                Rect::new(
                    inner_x,
                    right.y + pad_top + i as f32 * (item_h + gap),
                    inner_w,
                    item_h,
                )
            })
            .collect()
    }
}

struct ActionLayout {
    title_y: f32,
    open: Rect,
    clone: Rect,
    init: Rect,
    path_label_y: f32,
    path_input: Rect,
    path_open_btn: Rect,
}

impl Widget for WelcomeView {
    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        let (left, right) = Self::split_columns(bounds);
        let actions = Self::action_layout(left);
        let recent_rects = Self::recent_item_bounds(right);

        // Keyboard
        if let InputEvent::KeyDown { key, .. } = event
            && *key == Key::Enter
            && self.path_input.is_focused()
        {
            self.submit_path_input();
            return EventResponse::Consumed;
        }

        // Hover tracking for recent items (also clears path-input focus indicator on move)
        if let InputEvent::MouseMove { x, y, .. } = event {
            self.hovered_recent = -1;
            for (i, r) in recent_rects.iter().enumerate() {
                if i >= self.recent.len() {
                    break;
                }
                if r.contains(*x, *y) {
                    self.hovered_recent = i as i32;
                    break;
                }
            }
        }

        // Recent click → open
        if let InputEvent::MouseDown {
            button: MouseButton::Left,
            x,
            y,
            ..
        } = event
        {
            for (i, r) in recent_rects.iter().enumerate() {
                if i >= self.recent.len() {
                    break;
                }
                if r.contains(*x, *y) {
                    self.pending_action = Some(WelcomeAction::Open(self.recent[i].path.clone()));
                    return EventResponse::Consumed;
                }
            }
        }

        // Action buttons
        if self
            .open_button
            .handle_event(event, actions.open)
            .is_consumed()
        {
            if self.open_button.was_clicked() {
                self.spawn_picker("Open Git Repository", true);
            }
            return EventResponse::Consumed;
        }
        if self
            .clone_button
            .handle_event(event, actions.clone)
            .is_consumed()
        {
            if self.clone_button.was_clicked() {
                self.pending_action = Some(WelcomeAction::Clone);
            }
            return EventResponse::Consumed;
        }
        if self
            .init_button
            .handle_event(event, actions.init)
            .is_consumed()
        {
            if self.init_button.was_clicked() {
                self.spawn_picker("Initialize Repository In…", false);
            }
            return EventResponse::Consumed;
        }

        // Path input + Open button
        if self
            .path_input
            .handle_event(event, actions.path_input)
            .is_consumed()
        {
            return EventResponse::Consumed;
        }
        if self
            .open_path_button
            .handle_event(event, actions.path_open_btn)
            .is_consumed()
        {
            if self.open_path_button.was_clicked() {
                self.submit_path_input();
            }
            return EventResponse::Consumed;
        }

        EventResponse::Ignored
    }

    fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        self.layout_with_bold(text_renderer, text_renderer, bounds)
    }
}

impl WelcomeView {
    pub fn layout_with_bold(
        &self,
        text_renderer: &TextRenderer,
        bold_renderer: &TextRenderer,
        bounds: Rect,
    ) -> WidgetOutput {
        let mut output = WidgetOutput::new();
        let (left, right) = Self::split_columns(bounds);
        let actions = Self::action_layout(left);
        let line_height = text_renderer.line_height();

        // Surface fills (subtle) — slightly raised left column to suggest sidebar
        output.spline_vertices.extend(create_rect_vertices(
            &left,
            theme::SURFACE_RAISED.with_alpha(0.5).to_array(),
        ));
        // Vertical separator between columns
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(right.x, right.y, 1.0, right.height),
            theme::BORDER.with_alpha(0.5).to_array(),
        ));

        // ── Left: title and actions ────────────────────────────────────────
        output.bold_text_vertices.extend(bold_renderer.layout_text(
            "Whisper Git",
            actions.open.x,
            actions.title_y,
            theme::TEXT_BRIGHT.to_array(),
        ));
        output.text_vertices.extend(text_renderer.layout_text(
            "GPU-accelerated Git client",
            actions.open.x,
            actions.title_y + line_height + 2.0,
            theme::TEXT_MUTED.to_array(),
        ));

        output.extend(self.open_button.layout_with_bold(
            text_renderer,
            bold_renderer,
            actions.open,
        ));
        output.extend(self.clone_button.layout_with_bold(
            text_renderer,
            bold_renderer,
            actions.clone,
        ));
        output.extend(self.init_button.layout_with_bold(
            text_renderer,
            bold_renderer,
            actions.init,
        ));

        // Path input section
        output.text_vertices.extend(text_renderer.layout_text(
            "Open by path",
            actions.open.x,
            actions.path_label_y,
            theme::TEXT_MUTED.to_array(),
        ));
        output.extend(self.path_input.layout(text_renderer, actions.path_input));
        output.extend(self.open_path_button.layout_with_bold(
            text_renderer,
            bold_renderer,
            actions.path_open_btn,
        ));

        // Hint at bottom of the left column
        let hint_y = left.bottom() - 28.0;
        output.text_vertices.extend(text_renderer.layout_text(
            "Drop a folder anywhere to open",
            actions.open.x,
            hint_y,
            theme::TEXT_MUTED.with_alpha(0.7).to_array(),
        ));

        // ── Right: recent ──────────────────────────────────────────────────
        let recent_pad_x = 24.0;
        let recent_x = right.x + recent_pad_x;
        let header_y = right.y + 28.0;
        output.bold_text_vertices.extend(bold_renderer.layout_text(
            "Recent",
            recent_x,
            header_y,
            theme::TEXT.to_array(),
        ));
        // Subtle underline under the header
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(
                recent_x,
                header_y + line_height + 6.0,
                right.width - recent_pad_x * 2.0,
                1.0,
            ),
            theme::BORDER.with_alpha(0.4).to_array(),
        ));

        let item_rects = Self::recent_item_bounds(right);
        if self.recent.is_empty() {
            output.text_vertices.extend(text_renderer.layout_text(
                "No recent repositories.",
                recent_x,
                header_y + line_height * 3.0,
                theme::TEXT_MUTED.to_array(),
            ));
            output.text_vertices.extend(text_renderer.layout_text(
                "Use Open Local, Clone Remote, Init New, or drop a folder here.",
                recent_x,
                header_y + line_height * 4.5,
                theme::TEXT_MUTED.with_alpha(0.7).to_array(),
            ));
        } else {
            for (i, r) in item_rects.iter().enumerate() {
                if i >= self.recent.len() {
                    break;
                }
                let entry = &self.recent[i];
                let hovered = self.hovered_recent == i as i32;

                // Tile background
                let bg = if hovered {
                    theme::SURFACE_HOVER
                } else {
                    theme::SURFACE_RAISED.with_alpha(0.6)
                };
                output
                    .spline_vertices
                    .extend(create_rounded_rect_vertices(r, bg.to_array(), 6.0));

                // Name (bold) + parent (muted), each truncated to fit the tile.
                let name_color = if hovered {
                    theme::TEXT_BRIGHT
                } else {
                    theme::TEXT
                };
                let text_max_w = r.width - 28.0;
                let name_display = truncate_to_width(&entry.name, bold_renderer, text_max_w);
                output.bold_text_vertices.extend(bold_renderer.layout_text(
                    &name_display,
                    r.x + 14.0,
                    r.y + 10.0,
                    name_color.to_array(),
                ));
                let parent_display =
                    truncate_to_width(&abbreviate_home(&entry.parent), text_renderer, text_max_w);
                output.text_vertices.extend(text_renderer.layout_text(
                    &parent_display,
                    r.x + 14.0,
                    r.y + 10.0 + line_height + 6.0,
                    theme::TEXT_MUTED.to_array(),
                ));
            }
        }

        output
    }
}

/// Replace a leading $HOME with `~` for compactness.
fn abbreviate_home(path: &str) -> String {
    if let Ok(home) = std::env::var("HOME")
        && let Some(rest) = path.strip_prefix(&home)
    {
        return format!("~{rest}");
    }
    path.to_string()
}
