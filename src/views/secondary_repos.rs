//! Secondary repos view - displays submodules and worktrees
//! Note: This view is no longer rendered in the default layout (data moved to sidebar),
//! but kept for potential future use.

use crate::git::{SubmoduleInfo, WorktreeInfo};
use crate::ui::widget::{create_rect_vertices, create_rect_outline_vertices, theme, WidgetOutput};
use crate::ui::{Rect, TextRenderer};

/// A view displaying submodules and worktrees
#[allow(dead_code)]
pub struct SecondaryReposView {
    /// Submodules in the repository
    pub submodules: Vec<SubmoduleInfo>,
    /// Worktrees in the repository
    pub worktrees: Vec<WorktreeInfo>,
}

#[allow(dead_code)]
impl SecondaryReposView {
    pub fn new() -> Self {
        Self {
            submodules: Vec::new(),
            worktrees: Vec::new(),
        }
    }

    /// Update the submodules list
    pub fn set_submodules(&mut self, submodules: Vec<SubmoduleInfo>) {
        self.submodules = submodules;
    }

    /// Update the worktrees list
    pub fn set_worktrees(&mut self, worktrees: Vec<WorktreeInfo>) {
        self.worktrees = worktrees;
    }

    /// Layout the secondary repos view
    pub fn layout(&self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        // Panel background
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            theme::SURFACE.to_array(),
        ));

        // Panel border
        output.spline_vertices.extend(create_rect_outline_vertices(
            &bounds,
            theme::BORDER.to_array(),
            1.0,
        ));

        let padding = 12.0;
        let inner = bounds.inset(padding);
        let line_height = text_renderer.line_height() * 1.2;
        let section_gap = 12.0;
        let card_gap = 6.0;
        let card_padding = 8.0;

        let mut y = inner.y;

        // Title
        output.text_vertices.extend(text_renderer.layout_text(
            "Related Repositories",
            inner.x,
            y,
            theme::TEXT.to_array(),
        ));
        y += line_height + 4.0;

        // Title underline
        let underline_rect = Rect::new(inner.x, y, inner.width, 1.0);
        output.spline_vertices.extend(create_rect_vertices(
            &underline_rect,
            theme::BORDER.to_array(),
        ));
        y += section_gap;

        // Check if we have anything to show
        let has_content = !self.submodules.is_empty() || !self.worktrees.is_empty();

        if !has_content {
            // Empty state - centered message
            let empty_msg = "No submodules or worktrees";
            let msg_width = text_renderer.measure_text(empty_msg);
            output.text_vertices.extend(text_renderer.layout_text(
                empty_msg,
                inner.x + (inner.width - msg_width) / 2.0,
                y + 20.0,
                theme::TEXT_MUTED.to_array(),
            ));
            return output;
        }

        // Calculate card height based on content
        let card_height = line_height * 2.0 + card_padding * 2.0;

        // Submodules section
        if !self.submodules.is_empty() {
            // Section header with count badge
            let header = "SUBMODULES";
            output.text_vertices.extend(text_renderer.layout_text(
                header,
                inner.x,
                y,
                theme::TEXT_MUTED.to_array(),
            ));

            // Count badge
            let count_text = format!("{}", self.submodules.len());
            let header_width = text_renderer.measure_text(header);
            output.text_vertices.extend(text_renderer.layout_text(
                &count_text,
                inner.x + header_width + 8.0,
                y,
                theme::BRANCH_FEATURE.to_array(),
            ));

            y += line_height + card_gap;

            // Submodule cards
            for sm in &self.submodules {
                if y + card_height > inner.bottom() {
                    // Show overflow indicator
                    output.text_vertices.extend(text_renderer.layout_text(
                        "...",
                        inner.x,
                        y,
                        theme::TEXT_MUTED.to_array(),
                    ));
                    break;
                }

                let card_bounds = Rect::new(inner.x, y, inner.width, card_height);
                output.extend(self.layout_submodule_card(text_renderer, &card_bounds, sm));
                y += card_height + card_gap;
            }

            y += section_gap;
        }

        // Worktrees section
        if !self.worktrees.is_empty() {
            // Section header with count badge
            let header = "WORKTREES";
            output.text_vertices.extend(text_renderer.layout_text(
                header,
                inner.x,
                y,
                theme::TEXT_MUTED.to_array(),
            ));

            // Count badge
            let count_text = format!("{}", self.worktrees.len());
            let header_width = text_renderer.measure_text(header);
            output.text_vertices.extend(text_renderer.layout_text(
                &count_text,
                inner.x + header_width + 8.0,
                y,
                theme::BRANCH_RELEASE.to_array(),
            ));

            y += line_height + card_gap;

            // Worktree cards
            for wt in &self.worktrees {
                if y + card_height > inner.bottom() {
                    // Show overflow indicator
                    output.text_vertices.extend(text_renderer.layout_text(
                        "...",
                        inner.x,
                        y,
                        theme::TEXT_MUTED.to_array(),
                    ));
                    break;
                }

                let card_bounds = Rect::new(inner.x, y, inner.width, card_height);
                output.extend(self.layout_worktree_card(text_renderer, &card_bounds, wt));
                y += card_height + card_gap;
            }
        }

        output
    }

    /// Layout a single submodule card
    fn layout_submodule_card(
        &self,
        text_renderer: &TextRenderer,
        bounds: &Rect,
        sm: &SubmoduleInfo,
    ) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        let accent_width = 3.0;
        let card_padding = 8.0;
        let line_height = text_renderer.line_height() * 1.2;

        // Card background
        output.spline_vertices.extend(create_rect_vertices(
            bounds,
            theme::SURFACE_RAISED.to_array(),
        ));

        // Left accent border (green for submodules)
        let accent_rect = Rect::new(bounds.x, bounds.y, accent_width, bounds.height);
        output.spline_vertices.extend(create_rect_vertices(
            &accent_rect,
            theme::BRANCH_FEATURE.to_array(),
        ));

        let content_x = bounds.x + card_padding + accent_width + 2.0;
        let content_width = bounds.width - card_padding * 2.0 - accent_width - 2.0;
        let mut y = bounds.y + card_padding;

        // First line: Name + Status indicator
        // Dirty indicator (right-aligned)
        let status_width = if sm.is_dirty {
            let dirty_text = "modified";
            let width = text_renderer.measure_text(dirty_text);
            output.text_vertices.extend(text_renderer.layout_text(
                dirty_text,
                bounds.right() - card_padding - width,
                y,
                theme::STATUS_DIRTY.to_array(),
            ));
            width + 8.0
        } else {
            0.0
        };

        // Name (primary text)
        let name = truncate_text(&sm.name, text_renderer, content_width - status_width);
        output.text_vertices.extend(text_renderer.layout_text(
            &name,
            content_x,
            y,
            theme::TEXT_BRIGHT.to_array(),
        ));

        y += line_height;

        // Second line: Branch and path
        let branch_display = format!("@ {}", sm.branch);
        output.text_vertices.extend(text_renderer.layout_text(
            &branch_display,
            content_x,
            y,
            theme::BRANCH_PRIMARY.to_array(),
        ));

        // Path (right portion, muted)
        let branch_width = text_renderer.measure_text(&branch_display) + 16.0;
        let path_max_width = content_width - branch_width;
        if path_max_width > 50.0 {
            let path = truncate_path(&sm.path, text_renderer, path_max_width);
            let path_width = text_renderer.measure_text(&path);
            output.text_vertices.extend(text_renderer.layout_text(
                &path,
                bounds.right() - card_padding - path_width,
                y,
                theme::TEXT_MUTED.to_array(),
            ));
        }

        output
    }

    /// Layout a single worktree card
    fn layout_worktree_card(
        &self,
        text_renderer: &TextRenderer,
        bounds: &Rect,
        wt: &WorktreeInfo,
    ) -> WidgetOutput {
        let mut output = WidgetOutput::new();

        let accent_width = 3.0;
        let card_padding = 8.0;
        let line_height = text_renderer.line_height() * 1.2;

        // Card background
        output.spline_vertices.extend(create_rect_vertices(
            bounds,
            theme::SURFACE_RAISED.to_array(),
        ));

        // Left accent border (orange for worktrees)
        let accent_rect = Rect::new(bounds.x, bounds.y, accent_width, bounds.height);
        output.spline_vertices.extend(create_rect_vertices(
            &accent_rect,
            theme::BRANCH_RELEASE.to_array(),
        ));

        let content_x = bounds.x + card_padding + accent_width + 2.0;
        let content_width = bounds.width - card_padding * 2.0 - accent_width - 2.0;
        let mut y = bounds.y + card_padding;

        // First line: Name + current indicator
        let current_width = if wt.is_current {
            let current_text = "(current)";
            let width = text_renderer.measure_text(current_text);
            output.text_vertices.extend(text_renderer.layout_text(
                current_text,
                bounds.right() - card_padding - width,
                y,
                theme::ACCENT.to_array(),
            ));
            width + 8.0
        } else {
            0.0
        };

        let name = truncate_text(&wt.name, text_renderer, content_width - current_width);
        output.text_vertices.extend(text_renderer.layout_text(
            &name,
            content_x,
            y,
            theme::TEXT_BRIGHT.to_array(),
        ));

        y += line_height;

        // Second line: Branch and path
        let branch_display = format!("@ {}", wt.branch);
        output.text_vertices.extend(text_renderer.layout_text(
            &branch_display,
            content_x,
            y,
            theme::BRANCH_PRIMARY.to_array(),
        ));

        // Path (right portion, muted)
        let branch_width = text_renderer.measure_text(&branch_display) + 16.0;
        let path_max_width = content_width - branch_width;
        if path_max_width > 50.0 {
            let path = truncate_path(&wt.path, text_renderer, path_max_width);
            let path_width = text_renderer.measure_text(&path);
            output.text_vertices.extend(text_renderer.layout_text(
                &path,
                bounds.right() - card_padding - path_width,
                y,
                theme::TEXT_MUTED.to_array(),
            ));
        }

        output
    }
}

impl Default for SecondaryReposView {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
/// Truncate text to fit within max_width, adding ellipsis if needed
fn truncate_text(text: &str, text_renderer: &TextRenderer, max_width: f32) -> String {
    if max_width <= 0.0 {
        return String::new();
    }

    let full_width = text_renderer.measure_text(text);
    if full_width <= max_width {
        return text.to_string();
    }

    let ellipsis = "…";
    let ellipsis_width = text_renderer.measure_text(ellipsis);
    let target_width = max_width - ellipsis_width;

    if target_width <= 0.0 {
        return ellipsis.to_string();
    }

    // Binary search for the right truncation point
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

#[allow(dead_code)]
/// Truncate a path, preferring to keep the end (filename/leaf)
fn truncate_path(path: &str, text_renderer: &TextRenderer, max_width: f32) -> String {
    if max_width <= 0.0 {
        return String::new();
    }

    let full_width = text_renderer.measure_text(path);
    if full_width <= max_width {
        return path.to_string();
    }

    let ellipsis = "…/";
    let ellipsis_width = text_renderer.measure_text(ellipsis);
    let target_width = max_width - ellipsis_width;

    if target_width <= 0.0 {
        return "…".to_string();
    }

    // Try to keep meaningful path components from the end
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    // Start from the end and add components until we exceed target width
    let mut result = String::new();
    for part in parts.iter().rev() {
        let candidate = if result.is_empty() {
            part.to_string()
        } else {
            format!("{}/{}", part, result)
        };

        if text_renderer.measure_text(&candidate) > target_width {
            break;
        }
        result = candidate;
    }

    if result.is_empty() {
        // Path component is too long, truncate the last one
        if let Some(last) = parts.last() {
            return truncate_text(last, text_renderer, max_width);
        }
        return "…".to_string();
    }

    if result != path {
        format!("{}{}", ellipsis, result)
    } else {
        result
    }
}
