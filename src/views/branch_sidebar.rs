//! Branch sidebar view - displays local branches, remote branches, and tags

use std::collections::HashMap;

use crate::git::{BranchTip, TagInfo};
use crate::input::{EventResponse, InputEvent};
use crate::ui::widget::{create_rect_vertices, theme, WidgetOutput};
use crate::ui::{Rect, TextRenderer};

/// A sidebar showing local branches, remote branches, and tags
pub struct BranchSidebar {
    /// Local branch names
    pub local_branches: Vec<String>,
    /// Remote branch names grouped by remote (e.g., "origin" -> ["main", "feature"])
    pub remote_branches: HashMap<String, Vec<String>>,
    /// Tag names
    pub tags: Vec<String>,
    /// Current branch name (for highlighting)
    pub current_branch: String,
    /// Whether the LOCAL section is collapsed
    pub local_collapsed: bool,
    /// Whether the REMOTE section is collapsed
    pub remote_collapsed: bool,
    /// Whether the TAGS section is collapsed
    pub tags_collapsed: bool,
    /// Scroll offset for the sidebar content
    pub scroll_offset: f32,
}

impl BranchSidebar {
    pub fn new() -> Self {
        Self {
            local_branches: Vec::new(),
            remote_branches: HashMap::new(),
            tags: Vec::new(),
            current_branch: String::new(),
            local_collapsed: false,
            remote_collapsed: false,
            tags_collapsed: false,
            scroll_offset: 0.0,
        }
    }

    /// Populate from branch tips and tags from the git repo
    pub fn set_branch_data(
        &mut self,
        branch_tips: &[BranchTip],
        tags: &[TagInfo],
        current_branch: String,
    ) {
        self.current_branch = current_branch;

        // Separate local and remote branches
        self.local_branches.clear();
        self.remote_branches.clear();

        for tip in branch_tips {
            if tip.is_remote {
                // Parse "origin/main" into remote="origin", branch="main"
                if let Some((remote, branch)) = tip.name.split_once('/') {
                    self.remote_branches
                        .entry(remote.to_string())
                        .or_default()
                        .push(branch.to_string());
                }
            } else {
                self.local_branches.push(tip.name.clone());
            }
        }

        // Sort for consistent display
        self.local_branches.sort();
        for branches in self.remote_branches.values_mut() {
            branches.sort();
        }

        self.tags = tags.iter().map(|t| t.name.clone()).collect();
        self.tags.sort();
    }

    /// Handle input events (scrolling, clicking section headers)
    pub fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        match event {
            InputEvent::Scroll { delta_y, x, y, .. } => {
                if bounds.contains(*x, *y) {
                    self.scroll_offset = (self.scroll_offset - delta_y).max(0.0);
                    return EventResponse::Consumed;
                }
            }
            InputEvent::MouseDown { x, y, .. } => {
                if bounds.contains(*x, *y) {
                    // Check if click is on a section header
                    let padding = 8.0;
                    let inner = bounds.inset(padding);
                    let line_height = 18.0;
                    let section_header_height = 24.0;

                    let mut header_y = inner.y - self.scroll_offset;

                    // LOCAL header
                    if *y >= header_y && *y < header_y + section_header_height {
                        self.local_collapsed = !self.local_collapsed;
                        return EventResponse::Consumed;
                    }
                    header_y += section_header_height;
                    if !self.local_collapsed {
                        header_y += self.local_branches.len() as f32 * line_height;
                    }
                    header_y += 8.0; // gap between sections

                    // REMOTE header
                    if *y >= header_y && *y < header_y + section_header_height {
                        self.remote_collapsed = !self.remote_collapsed;
                        return EventResponse::Consumed;
                    }
                    header_y += section_header_height;
                    if !self.remote_collapsed {
                        for (_remote, branches) in &self.remote_branches {
                            header_y += line_height; // remote name sub-header
                            header_y += branches.len() as f32 * line_height;
                        }
                    }
                    header_y += 8.0;

                    // TAGS header
                    if *y >= header_y && *y < header_y + section_header_height {
                        self.tags_collapsed = !self.tags_collapsed;
                        return EventResponse::Consumed;
                    }

                    return EventResponse::Consumed;
                }
            }
            _ => {}
        }
        EventResponse::Ignored
    }

    /// Layout the sidebar and produce rendering output
    pub fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        // Panel background
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            theme::SURFACE.to_array(),
        ));

        // Panel border (right edge only)
        let border_rect = Rect::new(bounds.right() - 1.0, bounds.y, 1.0, bounds.height);
        output.spline_vertices.extend(create_rect_vertices(
            &border_rect,
            theme::BORDER.to_array(),
        ));

        let padding = 8.0;
        let inner = bounds.inset(padding);
        let line_height = 18.0;
        let section_header_height = 24.0;
        let indent = 12.0;
        let section_gap = 8.0;

        let mut y = inner.y - self.scroll_offset;

        // --- LOCAL section ---
        y = self.layout_section_header(
            text_renderer,
            &mut output,
            &inner,
            y,
            "LOCAL",
            self.local_branches.len(),
            self.local_collapsed,
            section_header_height,
        );

        if !self.local_collapsed {
            for branch in &self.local_branches {
                if y >= bounds.bottom() {
                    break;
                }
                if y + line_height > bounds.y {
                    let is_current = *branch == self.current_branch;

                    if is_current {
                        // Highlight background for current branch
                        let highlight_rect = Rect::new(
                            inner.x,
                            y,
                            inner.width,
                            line_height,
                        );
                        output.spline_vertices.extend(create_rect_vertices(
                            &highlight_rect,
                            theme::ACCENT_MUTED.to_array(),
                        ));
                    }

                    let color = if is_current {
                        theme::ACCENT.to_array()
                    } else {
                        theme::TEXT.to_array()
                    };

                    let display_name = truncate_to_width(branch, text_renderer, inner.width - indent);
                    output.text_vertices.extend(text_renderer.layout_text(
                        &display_name,
                        inner.x + indent,
                        y + 2.0,
                        color,
                    ));
                }
                y += line_height;
            }
        }

        y += section_gap;

        // --- REMOTE section ---
        let remote_count: usize = self.remote_branches.values().map(|v| v.len()).sum();
        y = self.layout_section_header(
            text_renderer,
            &mut output,
            &inner,
            y,
            "REMOTE",
            remote_count,
            self.remote_collapsed,
            section_header_height,
        );

        if !self.remote_collapsed {
            // Sort remote names for consistent order
            let mut remote_names: Vec<&String> = self.remote_branches.keys().collect();
            remote_names.sort();

            for remote_name in remote_names {
                let branches = &self.remote_branches[remote_name];
                if y >= bounds.bottom() {
                    break;
                }

                // Remote name sub-header
                if y + line_height > bounds.y {
                    output.text_vertices.extend(text_renderer.layout_text(
                        remote_name,
                        inner.x + indent,
                        y + 2.0,
                        theme::TEXT_MUTED.to_array(),
                    ));
                }
                y += line_height;

                // Branches under this remote
                for branch in branches {
                    if y >= bounds.bottom() {
                        break;
                    }
                    if y + line_height > bounds.y {
                        let display_name = truncate_to_width(branch, text_renderer, inner.width - indent * 2.0);
                        output.text_vertices.extend(text_renderer.layout_text(
                            &display_name,
                            inner.x + indent * 2.0,
                            y + 2.0,
                            theme::BRANCH_REMOTE.to_array(),
                        ));
                    }
                    y += line_height;
                }
            }
        }

        y += section_gap;

        // --- TAGS section ---
        y = self.layout_section_header(
            text_renderer,
            &mut output,
            &inner,
            y,
            "TAGS",
            self.tags.len(),
            self.tags_collapsed,
            section_header_height,
        );

        if !self.tags_collapsed {
            for tag in &self.tags {
                if y >= bounds.bottom() {
                    break;
                }
                if y + line_height > bounds.y {
                    let display_name = truncate_to_width(tag, text_renderer, inner.width - indent);
                    output.text_vertices.extend(text_renderer.layout_text(
                        &display_name,
                        inner.x + indent,
                        y + 2.0,
                        theme::BRANCH_RELEASE.to_array(),
                    ));
                }
                y += line_height;
            }
        }

        output
    }

    /// Layout a section header (e.g., "LOCAL  3") and return the new y position
    fn layout_section_header(
        &self,
        text_renderer: &TextRenderer,
        output: &mut WidgetOutput,
        inner: &Rect,
        y: f32,
        title: &str,
        count: usize,
        collapsed: bool,
        header_height: f32,
    ) -> f32 {
        // Section header background
        let header_rect = Rect::new(inner.x, y, inner.width, header_height);
        output.spline_vertices.extend(create_rect_vertices(
            &header_rect,
            theme::SURFACE_RAISED.to_array(),
        ));

        // Collapse indicator
        let indicator = if collapsed { "+" } else { "-" };
        output.text_vertices.extend(text_renderer.layout_text(
            indicator,
            inner.x + 4.0,
            y + 4.0,
            theme::TEXT_MUTED.to_array(),
        ));

        // Section title
        output.text_vertices.extend(text_renderer.layout_text(
            title,
            inner.x + 16.0,
            y + 4.0,
            theme::TEXT_MUTED.to_array(),
        ));

        // Count badge
        let count_text = format!("{}", count);
        let title_width = text_renderer.measure_text(title);
        output.text_vertices.extend(text_renderer.layout_text(
            &count_text,
            inner.x + 16.0 + title_width + 8.0,
            y + 4.0,
            theme::BRANCH_FEATURE.to_array(),
        ));

        y + header_height
    }
}

impl Default for BranchSidebar {
    fn default() -> Self {
        Self::new()
    }
}

/// Truncate text to fit within max_width, adding ellipsis if needed
fn truncate_to_width(text: &str, text_renderer: &TextRenderer, max_width: f32) -> String {
    if max_width <= 0.0 {
        return String::new();
    }

    let full_width = text_renderer.measure_text(text);
    if full_width <= max_width {
        return text.to_string();
    }

    let ellipsis = "...";
    let ellipsis_width = text_renderer.measure_text(ellipsis);
    let target_width = max_width - ellipsis_width;

    if target_width <= 0.0 {
        return ellipsis.to_string();
    }

    let chars: Vec<char> = text.chars().collect();
    let mut end = chars.len();

    while end > 0 {
        let truncated: String = chars[..end].iter().collect();
        if text_renderer.measure_text(&truncated) <= target_width {
            return format!("{}{}", truncated, ellipsis);
        }
        end -= 1;
    }

    ellipsis.to_string()
}
