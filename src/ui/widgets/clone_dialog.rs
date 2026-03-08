//! Clone repository dialog - modal overlay for cloning remote repos

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use winit::event_loop::EventLoopProxy;

use crate::github::RepoInfo;
use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::text_util::truncate_to_width;
use crate::ui::widget::{
    Widget, WidgetOutput, create_dialog_backdrop, create_rect_vertices,
    create_rounded_rect_outline_vertices, create_rounded_rect_vertices, theme,
};
use crate::ui::widgets::{Button, TextInput};
use crate::ui::{Rect, TextRenderer};

/// Actions from the clone dialog
#[derive(Clone, Debug)]
pub enum CloneDialogAction {
    /// Clone the given URL into the given directory
    Clone {
        url: String,
        dest: PathBuf,
        bare: bool,
    },
    Cancel,
}

/// A modal dialog for cloning a remote repository
pub struct CloneDialog {
    visible: bool,
    url_input: TextInput,
    dest_input: TextInput,
    browse_button: Button,
    clone_button: Button,
    cancel_button: Button,
    error_message: Option<String>,
    pending_action: Option<CloneDialogAction>,
    /// Channel receiver for async native folder picker
    picker_rx: Option<Receiver<String>>,
    proxy: Option<EventLoopProxy<()>>,
    /// Whether to clone as a bare repository
    bare: bool,

    // GitHub repo list
    /// Full list of repos from GitHub API
    github_repos: Vec<RepoInfo>,
    /// Filtered list indices (into github_repos) matching the current input
    filtered_indices: Vec<usize>,
    /// Scroll offset in pixels for the repo list
    scroll_offset: f32,
    /// Which list item is hovered (-1 = none)
    hovered_item: i32,
    /// Receiver for async GitHub repo list fetch
    repo_list_rx: Option<Receiver<anyhow::Result<Vec<RepoInfo>>>>,
    /// Whether we're currently fetching the repo list
    fetching_repos: bool,
}

/// Height of each repo list item
const ITEM_HEIGHT: f32 = 36.0;
/// Maximum visible items in the list area
const MAX_VISIBLE_ITEMS: usize = 8;

impl CloneDialog {
    pub fn new() -> Self {
        Self {
            visible: false,
            url_input: TextInput::new().with_placeholder("https://github.com/owner/repo.git"),
            dest_input: TextInput::new().with_placeholder("~/Projects"),
            browse_button: Button::new("Browse..."),
            clone_button: Button::new("Clone").primary(),
            cancel_button: Button::new("Cancel"),
            error_message: None,
            pending_action: None,
            picker_rx: None,
            proxy: None,
            bare: false,
            github_repos: Vec::new(),
            filtered_indices: Vec::new(),
            scroll_offset: 0.0,
            hovered_item: -1,
            repo_list_rx: None,
            fetching_repos: false,
        }
    }

    pub fn set_proxy(&mut self, proxy: EventLoopProxy<()>) {
        self.proxy = Some(proxy);
    }

    /// Show the dialog and optionally kick off a GitHub repo list fetch.
    pub fn show(&mut self, github_token: Option<&str>) {
        self.visible = true;
        self.url_input.set_text("");
        self.url_input.set_focused(true);
        self.dest_input.set_focused(false);
        self.error_message = None;
        self.bare = false;
        self.hovered_item = -1;
        self.scroll_offset = 0.0;
        self.filtered_indices.clear();

        // Set default destination
        let default_dest = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        self.dest_input.set_text(&default_dest);

        // Fetch GitHub repos if token is available and we don't already have them
        if self.github_repos.is_empty()
            && let Some(token) = github_token
            && !token.is_empty()
            && let Some(proxy) = &self.proxy
            && let Some(rx) = crate::github::fetch_repo_list_async(token, proxy.clone())
        {
            self.repo_list_rx = Some(rx);
            self.fetching_repos = true;
        } else if !self.github_repos.is_empty() {
            // Already have repos, just reset filter
            self.update_filter();
        }
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.url_input.set_focused(false);
        self.dest_input.set_focused(false);
        self.error_message = None;
        self.hovered_item = -1;
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn take_action(&mut self) -> Option<CloneDialogAction> {
        self.pending_action.take()
    }

    /// Poll async receivers. Call once per frame.
    pub fn poll(&mut self) {
        // Poll GitHub repo list
        if let Some(ref rx) = self.repo_list_rx {
            match rx.try_recv() {
                Ok(Ok(repos)) => {
                    self.github_repos = repos;
                    self.fetching_repos = false;
                    self.repo_list_rx = None;
                    self.update_filter();
                }
                Ok(Err(_)) => {
                    self.fetching_repos = false;
                    self.repo_list_rx = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.fetching_repos = false;
                    self.repo_list_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }

        // Poll folder picker
        if let Some(ref rx) = self.picker_rx {
            match rx.try_recv() {
                Ok(path) => {
                    self.dest_input.set_text(&path);
                    self.picker_rx = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.picker_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    /// Update filtered repo list based on current URL input text.
    fn update_filter(&mut self) {
        let query = self.url_input.text().trim().to_lowercase();
        self.scroll_offset = 0.0;
        self.hovered_item = -1;

        if query.is_empty() {
            // Show all repos when input is empty
            self.filtered_indices = (0..self.github_repos.len()).collect();
        } else {
            self.filtered_indices = self
                .github_repos
                .iter()
                .enumerate()
                .filter(|(_, repo)| {
                    repo.full_name.to_lowercase().contains(&query)
                        || repo
                            .description
                            .as_deref()
                            .is_some_and(|d| d.to_lowercase().contains(&query))
                })
                .map(|(i, _)| i)
                .collect();
        }
    }

    fn try_clone(&mut self) {
        let url = self.url_input.text().trim().to_string();
        if url.is_empty() {
            self.error_message = Some("Please enter a clone URL".into());
            return;
        }

        let dest_base = self.dest_input.text().trim().to_string();
        if dest_base.is_empty() {
            self.error_message = Some("Please enter a destination directory".into());
            return;
        }

        // Derive repo name from URL for the subdirectory
        let repo_name = url
            .rsplit('/')
            .next()
            .unwrap_or("repo")
            .strip_suffix(".git")
            .unwrap_or(url.rsplit('/').next().unwrap_or("repo"));
        // Bare repos conventionally use a .git suffix on the directory name
        let dir_name = if self.bare {
            format!("{repo_name}.git")
        } else {
            repo_name.to_string()
        };
        let dest_base = if let Some(rest) = dest_base.strip_prefix('~') {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{home}{rest}")
        } else {
            dest_base
        };
        let dest = PathBuf::from(&dest_base).join(dir_name);

        if dest.exists() {
            self.error_message = Some(format!("Directory already exists: {}", dest.display()));
            return;
        }

        self.pending_action = Some(CloneDialogAction::Clone {
            url,
            dest,
            bare: self.bare,
        });
        self.hide();
    }

    fn open_folder_picker(&mut self) {
        if self.picker_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let start_dir = {
            let t = self.dest_input.text().trim().to_string();
            if t.is_empty() {
                std::env::var("HOME").unwrap_or_else(|_| "/".into())
            } else if let Some(rest) = t.strip_prefix('~') {
                let home = std::env::var("HOME").unwrap_or_default();
                format!("{home}{rest}")
            } else {
                t
            }
        };
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            if let Some(folder) = rfd::FileDialog::new()
                .set_directory(&start_dir)
                .set_title("Clone Destination")
                .pick_folder()
            {
                let _ = tx.send(folder.to_string_lossy().to_string());
            }
            if let Some(p) = proxy {
                let _ = p.send_event(());
            }
        });
        self.picker_rx = Some(rx);
    }

    fn dialog_bounds(&self, screen: Rect, scale: f32) -> Rect {
        let list_h = if !self.github_repos.is_empty() || self.fetching_repos {
            (MAX_VISIBLE_ITEMS as f32 * ITEM_HEIGHT * scale) + 24.0 * scale
        } else {
            0.0
        };
        let checkbox_h = 24.0 * scale; // space for bare checkbox row
        let dialog_w = (500.0 * scale).min(screen.width * 0.85);
        let dialog_h = ((240.0 * scale) + checkbox_h + list_h).min(screen.height * 0.85);
        let dialog_x = screen.x + (screen.width - dialog_w) / 2.0;
        let dialog_y = screen.y + (screen.height - dialog_h) / 2.0;
        Rect::new(dialog_x, dialog_y, dialog_w, dialog_h)
    }

    /// Compute the visible area for the repo list.
    fn list_area(&self, dialog: Rect, scale: f32) -> Rect {
        let padding = 16.0 * scale;
        let line_h = 32.0 * scale;
        let url_y = dialog.y + 44.0 * scale;
        let dest_y = url_y + line_h + 8.0 * scale;
        let checkbox_y = dest_y + line_h + 8.0 * scale;
        let checkbox_h = 16.0 * scale;
        let list_top = checkbox_y + checkbox_h + 12.0 * scale;
        let button_area = padding + line_h + 8.0 * scale;
        let list_bottom = dialog.bottom() - button_area - 4.0 * scale;
        let list_h = (list_bottom - list_top).max(0.0);
        Rect::new(
            dialog.x + padding,
            list_top,
            dialog.width - padding * 2.0,
            list_h,
        )
    }
}

impl Widget for CloneDialog {
    fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        if !self.visible {
            return EventResponse::Ignored;
        }

        let scale = (bounds.height / 720.0).max(1.0);
        let dialog = self.dialog_bounds(bounds, scale);
        let padding = 16.0 * scale;
        let line_h = 32.0 * scale;

        // URL input bounds
        let url_y = dialog.y + 44.0 * scale;
        let url_bounds = Rect::new(
            dialog.x + padding,
            url_y,
            dialog.width - padding * 2.0,
            line_h,
        );

        // Destination input + browse button
        let dest_y = url_y + line_h + 8.0 * scale;
        let browse_w = 90.0 * scale;
        let browse_gap = 8.0 * scale;
        let dest_bounds = Rect::new(
            dialog.x + padding,
            dest_y,
            dialog.width - padding * 2.0 - browse_w - browse_gap,
            line_h,
        );
        let browse_bounds = Rect::new(dest_bounds.right() + browse_gap, dest_y, browse_w, line_h);

        // Bare checkbox bounds (between dest input and repo list)
        let checkbox_y = dest_y + line_h + 8.0 * scale;
        let checkbox_size = 16.0 * scale;
        let checkbox_bounds =
            Rect::new(dialog.x + padding, checkbox_y, checkbox_size, checkbox_size);

        // Button bounds at bottom
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let button_gap = 8.0 * scale;
        let cancel_x = dialog.right() - padding - button_w;
        let clone_x = cancel_x - button_w - button_gap;
        let clone_bounds = Rect::new(clone_x, button_y, button_w, line_h);
        let cancel_bounds = Rect::new(cancel_x, button_y, button_w, line_h);

        // Keyboard shortcuts
        if let InputEvent::KeyDown { key, .. } = event {
            match key {
                Key::Escape => {
                    self.pending_action = Some(CloneDialogAction::Cancel);
                    self.hide();
                    return EventResponse::Consumed;
                }
                Key::Enter => {
                    self.try_clone();
                    return EventResponse::Consumed;
                }
                _ => {}
            }
        }

        // Scroll the repo list
        let list_area = self.list_area(dialog, scale);
        if let InputEvent::Scroll { delta_y, x, y, .. } = event
            && list_area.contains(*x, *y)
        {
            let item_h = ITEM_HEIGHT * scale;
            let total_h = self.filtered_indices.len() as f32 * item_h;
            let max_scroll = (total_h - list_area.height).max(0.0);
            self.scroll_offset = (self.scroll_offset - delta_y).clamp(0.0, max_scroll);
            return EventResponse::Consumed;
        }

        // Mouse move for list item hover
        if let InputEvent::MouseMove { x, y, .. } = event {
            self.hovered_item = -1;
            if list_area.contains(*x, *y) {
                let item_h = ITEM_HEIGHT * scale;
                let rel_y = *y - list_area.y + self.scroll_offset;
                let idx = (rel_y / item_h) as i32;
                if idx >= 0 && (idx as usize) < self.filtered_indices.len() {
                    self.hovered_item = idx;
                }
            }
        }

        // Click on list item
        if let InputEvent::MouseDown {
            button: MouseButton::Left,
            x,
            y,
            ..
        } = event
            && list_area.contains(*x, *y)
        {
            let item_h = ITEM_HEIGHT * scale;
            let rel_y = *y - list_area.y + self.scroll_offset;
            let idx = (rel_y / item_h) as usize;
            if idx < self.filtered_indices.len() {
                let repo = &self.github_repos[self.filtered_indices[idx]];
                self.url_input.set_text(&repo.clone_url);
                self.error_message = None;
                return EventResponse::Consumed;
            }
        }

        // Track previous text for change detection
        let prev_url_text = self.url_input.text().to_string();

        // Route to URL input
        if self.url_input.handle_event(event, url_bounds).is_consumed() {
            self.error_message = None;
            // Update filter if text changed
            if self.url_input.text() != prev_url_text {
                self.update_filter();
            }
            return EventResponse::Consumed;
        }

        // Route to destination input
        if self
            .dest_input
            .handle_event(event, dest_bounds)
            .is_consumed()
        {
            return EventResponse::Consumed;
        }

        // Browse button
        if self
            .browse_button
            .handle_event(event, browse_bounds)
            .is_consumed()
        {
            if self.browse_button.was_clicked() {
                self.open_folder_picker();
            }
            return EventResponse::Consumed;
        }

        // Bare checkbox
        if let InputEvent::MouseDown {
            button: MouseButton::Left,
            x,
            y,
            ..
        } = event
            && checkbox_bounds.contains(*x, *y)
        {
            self.bare = !self.bare;
            return EventResponse::Consumed;
        }

        // Clone button
        if self
            .clone_button
            .handle_event(event, clone_bounds)
            .is_consumed()
        {
            if self.clone_button.was_clicked() {
                self.try_clone();
            }
            return EventResponse::Consumed;
        }

        // Cancel button
        if self
            .cancel_button
            .handle_event(event, cancel_bounds)
            .is_consumed()
        {
            if self.cancel_button.was_clicked() {
                self.pending_action = Some(CloneDialogAction::Cancel);
                self.hide();
            }
            return EventResponse::Consumed;
        }

        // Click outside dismisses
        if let InputEvent::MouseDown {
            button: MouseButton::Left,
            x,
            y,
            ..
        } = event
            && !dialog.contains(*x, *y)
        {
            self.pending_action = Some(CloneDialogAction::Cancel);
            self.hide();
            return EventResponse::Consumed;
        }

        EventResponse::Consumed
    }

    fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        self.layout_with_bold(text_renderer, text_renderer, bounds)
    }
}

impl CloneDialog {
    pub fn layout_with_bold(
        &self,
        text_renderer: &TextRenderer,
        bold_renderer: &TextRenderer,
        bounds: Rect,
    ) -> WidgetOutput {
        let mut output = WidgetOutput::new();
        if !self.visible {
            return output;
        }

        let scale = (bounds.height / 720.0).max(1.0);
        let dialog = self.dialog_bounds(bounds, scale);
        let padding = 16.0 * scale;
        let line_h = 32.0 * scale;
        let line_height = text_renderer.line_height();

        // Backdrop + dialog background
        create_dialog_backdrop(&mut output, &bounds, &dialog, scale);

        // Title
        let title_y = dialog.y + padding;
        output.bold_text_vertices.extend(bold_renderer.layout_text(
            "Clone Repository",
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

        // URL input
        let url_y = dialog.y + 44.0 * scale;
        let url_bounds = Rect::new(
            dialog.x + padding,
            url_y,
            dialog.width - padding * 2.0,
            line_h,
        );
        output.extend(self.url_input.layout(text_renderer, url_bounds));

        // Destination input + browse
        let dest_y = url_y + line_h + 8.0 * scale;
        let browse_w = 90.0 * scale;
        let browse_gap = 8.0 * scale;
        let dest_bounds = Rect::new(
            dialog.x + padding,
            dest_y,
            dialog.width - padding * 2.0 - browse_w - browse_gap,
            line_h,
        );
        let browse_bounds = Rect::new(dest_bounds.right() + browse_gap, dest_y, browse_w, line_h);
        output.extend(self.dest_input.layout(text_renderer, dest_bounds));
        output.extend(self.browse_button.layout(text_renderer, browse_bounds));

        // Error message
        if let Some(ref err) = self.error_message {
            let err_y = dest_y + line_h + 4.0 * scale;
            output.text_vertices.extend(text_renderer.layout_text(
                err,
                dialog.x + padding,
                err_y,
                theme::STATUS_DIRTY.to_array(),
            ));
        }

        // Bare checkbox
        let checkbox_y = dest_y + line_h + 8.0 * scale;
        let checkbox_size = 16.0 * scale;
        let checkbox_x = dialog.x + padding;
        let checkbox_rect = Rect::new(checkbox_x, checkbox_y, checkbox_size, checkbox_size);
        let cb_r = 3.0 * scale;
        let border_color = if self.bare {
            theme::ACCENT.to_array()
        } else {
            theme::BORDER.to_array()
        };
        output
            .spline_vertices
            .extend(create_rounded_rect_outline_vertices(
                &checkbox_rect,
                border_color,
                cb_r,
                1.0 * scale,
            ));
        if self.bare {
            let check_padding = 3.0 * scale;
            let check_rect = Rect::new(
                checkbox_x + check_padding,
                checkbox_y + check_padding,
                checkbox_size - check_padding * 2.0,
                checkbox_size - check_padding * 2.0,
            );
            output.spline_vertices.extend(create_rounded_rect_vertices(
                &check_rect,
                theme::ACCENT.to_array(),
                cb_r - 1.0,
            ));
        }
        let checkbox_label_x = checkbox_x + checkbox_size + 8.0 * scale;
        output.text_vertices.extend(text_renderer.layout_text(
            "Clone as bare repository (--bare)",
            checkbox_label_x,
            checkbox_y,
            theme::TEXT.to_array(),
        ));

        // GitHub repo list
        let list_area = self.list_area(dialog, scale);
        if list_area.height > 0.0 {
            if self.fetching_repos {
                let fetch_y = list_area.y + 4.0 * scale;
                output.text_vertices.extend(text_renderer.layout_text(
                    "Loading repositories...",
                    list_area.x,
                    fetch_y,
                    theme::TEXT_MUTED.to_array(),
                ));
            } else if !self.github_repos.is_empty() {
                // Section label
                let label_y = list_area.y - 16.0 * scale;
                let count = self.filtered_indices.len();
                let label = if self.url_input.text().trim().is_empty() {
                    format!("Your repositories ({count})")
                } else {
                    format!("Matching repositories ({count})")
                };
                output.text_vertices.extend(text_renderer.layout_text(
                    &label,
                    list_area.x,
                    label_y,
                    theme::TEXT_MUTED.to_array(),
                ));

                // List border
                output.spline_vertices.extend(create_rounded_rect_vertices(
                    &list_area,
                    theme::SURFACE.to_array(),
                    4.0 * scale,
                ));

                // Render visible items
                let item_h = ITEM_HEIGHT * scale;
                let first_visible = (self.scroll_offset / item_h) as usize;
                let visible_count = ((list_area.height / item_h).ceil() as usize + 1)
                    .min(self.filtered_indices.len().saturating_sub(first_visible));

                for i in 0..visible_count {
                    let list_idx = first_visible + i;
                    if list_idx >= self.filtered_indices.len() {
                        break;
                    }
                    let repo = &self.github_repos[self.filtered_indices[list_idx]];

                    let item_y = list_area.y + list_idx as f32 * item_h - self.scroll_offset;

                    // Skip items outside visible area
                    if item_y + item_h < list_area.y || item_y > list_area.bottom() {
                        continue;
                    }

                    let item_rect = Rect::new(list_area.x, item_y, list_area.width, item_h);

                    // Hover highlight
                    if self.hovered_item == list_idx as i32 {
                        output.spline_vertices.extend(create_rounded_rect_vertices(
                            &item_rect,
                            [1.0, 1.0, 1.0, 0.06],
                            4.0 * scale,
                        ));
                    }

                    let text_color = if self.hovered_item == list_idx as i32 {
                        theme::TEXT_BRIGHT
                    } else {
                        theme::TEXT
                    };

                    // Repo name (bold)
                    let name_y = item_y + 4.0 * scale;
                    let max_name_w = list_area.width - 8.0 * scale;
                    let name_display =
                        truncate_to_width(&repo.full_name, bold_renderer, max_name_w);
                    output.bold_text_vertices.extend(bold_renderer.layout_text(
                        &name_display,
                        list_area.x + 4.0 * scale,
                        name_y,
                        text_color.to_array(),
                    ));

                    // Description (muted, truncated)
                    if let Some(ref desc) = repo.description {
                        let desc_y = name_y + line_height + 1.0 * scale;
                        if desc_y + line_height <= item_y + item_h {
                            let desc_display = truncate_to_width(desc, text_renderer, max_name_w);
                            output.text_vertices.extend(text_renderer.layout_text(
                                &desc_display,
                                list_area.x + 4.0 * scale,
                                desc_y,
                                theme::TEXT_MUTED.to_array(),
                            ));
                        }
                    }

                    // Private/fork badges (right-aligned)
                    let mut badge_x = list_area.right() - 8.0 * scale;
                    if repo.private {
                        let badge = "private";
                        let badge_w = text_renderer.measure_text(badge);
                        badge_x -= badge_w;
                        output.text_vertices.extend(text_renderer.layout_text(
                            badge,
                            badge_x,
                            name_y,
                            theme::TEXT_MUTED.to_array(),
                        ));
                        badge_x -= 8.0 * scale;
                    }
                    if repo.fork {
                        let badge = "fork";
                        let badge_w = text_renderer.measure_text(badge);
                        badge_x -= badge_w;
                        output.text_vertices.extend(text_renderer.layout_text(
                            badge,
                            badge_x,
                            name_y,
                            theme::TEXT_MUTED.to_array(),
                        ));
                    }
                }

                // Scroll indicator (thin bar on right edge)
                if self.filtered_indices.len() as f32 * item_h > list_area.height {
                    let total_h = self.filtered_indices.len() as f32 * item_h;
                    let bar_h = (list_area.height / total_h * list_area.height).max(20.0 * scale);
                    let bar_y =
                        list_area.y + (self.scroll_offset / total_h) * (list_area.height - bar_h);
                    let bar_rect =
                        Rect::new(list_area.right() - 3.0 * scale, bar_y, 3.0 * scale, bar_h);
                    output.spline_vertices.extend(create_rounded_rect_vertices(
                        &bar_rect,
                        [1.0, 1.0, 1.0, 0.15],
                        1.5 * scale,
                    ));
                }
            }
        }

        // Buttons at bottom
        let button_y = dialog.bottom() - padding - line_h;
        let button_w = 80.0 * scale;
        let button_gap = 8.0 * scale;
        let cancel_x = dialog.right() - padding - button_w;
        let clone_x = cancel_x - button_w - button_gap;

        // Button separator
        let btn_sep_y = button_y - 8.0 * scale;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(
                dialog.x + padding,
                btn_sep_y,
                dialog.width - padding * 2.0,
                1.0,
            ),
            theme::BORDER.with_alpha(0.4).to_array(),
        ));

        let clone_bounds = Rect::new(clone_x, button_y, button_w, line_h);
        let cancel_bounds = Rect::new(cancel_x, button_y, button_w, line_h);

        output.extend(self.clone_button.layout(text_renderer, clone_bounds));
        output.extend(self.cancel_button.layout(text_renderer, cancel_bounds));

        // Hint
        let hint_y = button_y + (line_h - line_height) / 2.0;
        output.text_vertices.extend(text_renderer.layout_text(
            "Enter to clone",
            dialog.x + padding,
            hint_y,
            theme::TEXT_MUTED.to_array(),
        ));

        output
    }
}
