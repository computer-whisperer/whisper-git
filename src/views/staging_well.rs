//! Staging well view - commit message editor and file staging

use std::path::PathBuf;

use crate::git::{SubmoduleInfo, WorkingDirStatus, WorktreeInfo};
use crate::input::{EventResponse, InputEvent, Key};
use crate::ui::widget::{create_rect_vertices, create_rect_outline_vertices, create_rounded_rect_vertices, create_rounded_rect_outline_vertices, theme, WidgetOutput};
use crate::ui::widgets::context_menu::MenuItem;
use crate::ui::widgets::{Button, FileList, FileListAction, TextArea, TextInput};
use crate::ui::{Rect, TextRenderer, Widget};

/// Per-worktree state for the staging context switcher
#[derive(Clone, Debug)]
pub struct WorktreeContext {
    pub name: String,
    pub display_name: String,
    pub path: PathBuf,
    pub branch: String,
    pub is_current: bool,
    pub dirty_file_count: usize,
    pub subject_draft: String,
    pub body_draft: String,
}

/// Compute short display names by stripping the longest common prefix
/// (up to and including the last separator: `-`, `_`, or `/`).
pub(crate) fn compute_display_names(names: &[String]) -> Vec<String> {
    if names.len() < 2 {
        return names.to_vec();
    }
    // Find longest common prefix
    let first = &names[0];
    let prefix_len = first.len().min(
        names[1..].iter().map(|n| {
            first.chars().zip(n.chars()).take_while(|(a, b)| a == b).count()
        }).min().unwrap_or(0)
    );
    let common = &first[..prefix_len];
    // Walk backward to last separator
    let strip_len = common.rfind(|c: char| c == '-' || c == '_' || c == '/')
        .map(|i| i + 1) // include separator
        .unwrap_or(0);
    if strip_len == 0 {
        return names.to_vec();
    }
    let result: Vec<String> = names.iter().map(|n| n[strip_len..].to_string()).collect();
    // Guard: if any result is empty, return originals
    if result.iter().any(|s| s.is_empty()) {
        names.to_vec()
    } else {
        result
    }
}

/// Actions from the staging well
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StagingAction {
    StageFile(String),
    UnstageFile(String),
    StageAll,
    UnstageAll,
    Commit(String),
    AmendCommit(String),
    ToggleAmend,
    ViewDiff(String),
    SwitchWorktree(usize),
    /// Preview diff in the preview panel (file_path, is_staged)
    PreviewDiff(String, bool),
    /// Open (drill into) a submodule by name
    OpenSubmodule(String),
    /// Switch to a sibling submodule (exit current, then enter sibling)
    SwitchToSibling(String),
}

/// The staging well view containing commit message and file lists
pub struct StagingWell {
    /// Commit subject line input
    pub subject_input: TextInput,
    /// Commit body text area
    pub body_area: TextArea,
    /// Staged files list
    pub staged_list: FileList,
    /// Unstaged files list
    pub unstaged_list: FileList,
    /// Conflicted files list (shown during merge/rebase conflicts)
    pub conflicted_list: FileList,
    /// Stage all button
    stage_all_btn: Button,
    /// Unstage all button
    unstage_all_btn: Button,
    /// Commit button
    commit_btn: Button,
    /// Amend toggle button
    amend_btn: Button,
    /// Whether we are in amend mode
    pub amend_mode: bool,
    /// Pending action
    pending_action: Option<StagingAction>,
    /// Which section has focus (0=unstaged, 1=staged, 2=subject, 3=body)
    focus_section: usize,
    /// Display scale factor for 4K/HiDPI scaling
    pub scale: f32,
    /// Worktree contexts for multi-worktree repos
    worktree_contexts: Vec<WorktreeContext>,
    /// Index of the active worktree in worktree_contexts
    active_worktree_idx: usize,
    /// Individual pill rects in the pill bar (for click hit testing)
    pill_bar_rects: Vec<Rect>,
    /// Number of rows in the pill bar (for wrapping layout)
    cached_pill_rows: usize,
    /// Flag to request an immediate status refresh (e.g., after worktree switch)
    pub status_refresh_needed: bool,
    /// Submodule info for the current worktree/repo
    pub submodules: Vec<SubmoduleInfo>,
    /// Hit-test bounds for submodule rows: (rect, submodule_name)
    submodule_bounds: Vec<(Rect, String)>,
    /// Sibling submodules (parent's submodules for lateral navigation when inside a submodule)
    pub sibling_submodules: Vec<SubmoduleInfo>,
    /// Hit-test bounds for sibling submodule rows: (rect, submodule_name)
    sibling_bounds: Vec<(Rect, String)>,
    /// Current repo state label (e.g. "MERGE IN PROGRESS"), None when clean
    pub repo_state_label: Option<&'static str>,
}

/// Region layout results for the new top-to-bottom order:
/// unstaged files -> staged files -> commit message -> buttons
struct StagingRegions {
    unstaged_header: Rect,
    unstaged: Rect,
    staged_header: Rect,
    staged: Rect,
    /// Area for the submodule section (between staged and commit area, zero-height if none)
    submodules: Rect,
    /// Area for the sibling submodules section (after submodules, zero-height if none)
    siblings: Rect,
    commit_area: Rect,
    subject: Rect,
    body: Rect,
    buttons: Rect,
    stage_all_btn: Rect,
    unstage_all_btn: Rect,
    amend_btn: Rect,
    commit_btn: Rect,
}

impl StagingWell {
    pub fn new() -> Self {
        Self {
            subject_input: TextInput::new()
                .with_placeholder("Commit subject line...")
                .with_max_length(72),
            body_area: TextArea::new(),
            staged_list: { let mut fl = FileList::new("Staged", true); fl.hide_header = true; fl },
            unstaged_list: { let mut fl = FileList::new("Unstaged", false); fl.hide_header = true; fl },
            conflicted_list: { let mut fl = FileList::new("Conflicted", false); fl.hide_header = true; fl },
            stage_all_btn: Button::new("Stage All"),
            unstage_all_btn: Button::new("Unstage All"),
            commit_btn: Button::new("Commit").primary(),
            amend_btn: Button::new("Amend"),
            amend_mode: false,
            pending_action: None,
            focus_section: 0,
            scale: 1.0,
            worktree_contexts: Vec::new(),
            active_worktree_idx: 0,
            pill_bar_rects: Vec::new(),
            cached_pill_rows: 1,
            status_refresh_needed: false,
            submodules: Vec::new(),
            submodule_bounds: Vec::new(),
            sibling_submodules: Vec::new(),
            sibling_bounds: Vec::new(),
            repo_state_label: None,
        }
    }

    /// Set the display scale factor for HiDPI scaling. Propagates to child file lists.
    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale;
        self.staged_list.set_scale(scale);
        self.unstaged_list.set_scale(scale);
        self.conflicted_list.set_scale(scale);
    }

    /// Update from working directory status
    pub fn update_status(&mut self, status: &WorkingDirStatus) {
        use crate::ui::widgets::file_list::FileEntry;

        let staged: Vec<FileEntry> = status.staged.iter().map(FileEntry::from).collect();
        let unstaged: Vec<FileEntry> = status.unstaged.iter().map(FileEntry::from).collect();
        let conflicted: Vec<FileEntry> = status.conflicted.iter().map(FileEntry::from).collect();

        self.staged_list.set_files(staged);
        self.unstaged_list.set_files(unstaged);
        self.conflicted_list.set_files(conflicted);
    }

    /// Clear staging status (used when switching repos/submodules)
    pub fn clear_status(&mut self) {
        self.staged_list.set_files(Vec::new());
        self.unstaged_list.set_files(Vec::new());
    }

    /// Set the submodule list for the current repo/worktree.
    pub fn set_submodules(&mut self, subs: Vec<SubmoduleInfo>) {
        self.submodules = subs;
    }

    /// Set the sibling submodule list (parent's submodules for lateral navigation).
    pub fn set_sibling_submodules(&mut self, siblings: Vec<SubmoduleInfo>) {
        self.sibling_submodules = siblings;
    }

    /// Returns true if any button in the staging well is hovered
    pub fn is_any_button_hovered(&self) -> bool {
        self.stage_all_btn.is_hovered()
            || self.unstage_all_btn.is_hovered()
            || self.commit_btn.is_hovered()
            || self.amend_btn.is_hovered()
    }

    /// Returns true if a file in either list is hovered
    pub fn is_file_hovered(&self) -> bool {
        self.staged_list.is_item_hovered() || self.unstaged_list.is_item_hovered()
    }

    /// Get and clear any pending action
    pub fn take_action(&mut self) -> Option<StagingAction> {
        self.pending_action.take()
    }

    /// Get the commit message (subject + body)
    pub fn commit_message(&self) -> String {
        let subject = self.subject_input.text();
        let body = self.body_area.text();

        if body.trim().is_empty() {
            subject.to_string()
        } else {
            format!("{}\n\n{}", subject, body)
        }
    }

    /// Clear the commit message
    pub fn clear_message(&mut self) {
        self.subject_input.set_text("");
        self.body_area.set_text("");
    }

    /// Clear the commit message and focus the subject input field.
    /// Call this after a successful commit so the user can immediately type the next one.
    pub fn clear_and_focus(&mut self) {
        self.clear_message();
        self.focus_section = 2; // subject is now section 2
        self.update_focus_state();
    }

    /// Check if commit button should be enabled.
    /// In amend mode, we only require a non-empty subject (no staged files needed).
    pub fn can_commit(&self) -> bool {
        if self.amend_mode {
            !self.subject_input.text().trim().is_empty()
        } else {
            !self.subject_input.text().trim().is_empty() && !self.staged_list.files.is_empty()
        }
    }

    /// Enter amend mode, pre-filling the commit message with the last commit's subject/body.
    pub fn enter_amend_mode(&mut self, subject: &str, body: &str) {
        self.amend_mode = true;
        self.subject_input.set_text(subject);
        self.body_area.set_text(body);
        self.focus_section = 2; // subject is now section 2
        self.update_focus_state();
    }

    /// Exit amend mode and clear the commit message fields.
    pub fn exit_amend_mode(&mut self) {
        self.amend_mode = false;
        self.clear_message();
    }

    /// Get context menu items for the file at (x, y) in the staging area.
    /// Returns Some((items, is_staged_file)) if a file was right-clicked.
    pub fn context_menu_items_at(&self, x: f32, y: f32, bounds: Rect) -> Option<Vec<MenuItem>> {
        if !bounds.contains(x, y) {
            return None;
        }

        let (unstaged_bounds, staged_bounds, _, _, _) = self.compute_regions(bounds);

        // Check staged files
        if staged_bounds.contains(x, y)
            && let Some(file) = self.staged_list.file_at_y(y, staged_bounds) {
                let items = vec![
                    MenuItem::new("Unstage File", format!("unstage:{}", file)),
                    MenuItem::new("View Diff", format!("view_diff:{}", file)),
                ];
                return Some(items);
            }

        // Check unstaged files
        if unstaged_bounds.contains(x, y)
            && let Some(file) = self.unstaged_list.file_at_y(y, unstaged_bounds) {
                let items = vec![
                    MenuItem::new("Stage File", format!("stage:{}", file)),
                    MenuItem::new("View Diff", format!("view_diff:{}", file)),
                    MenuItem::new("Discard Changes", format!("discard:{}", file)),
                ];
                return Some(items);
            }

        None
    }

    fn cycle_focus(&mut self, forward: bool) {
        let sections = 4;
        if forward {
            self.focus_section = (self.focus_section + 1) % sections;
        } else {
            self.focus_section = (self.focus_section + sections - 1) % sections;
        }
        self.update_focus_state();
    }

    /// New focus mapping: 0=unstaged, 1=staged, 2=subject, 3=body
    fn update_focus_state(&mut self) {
        self.unstaged_list.set_focused(self.focus_section == 0);
        self.staged_list.set_focused(self.focus_section == 1);
        self.subject_input.set_focused(self.focus_section == 2);
        self.body_area.set_focused(self.focus_section == 3);
    }

    /// Check if any text input in the staging well has keyboard focus.
    pub fn has_text_focus(&self) -> bool {
        self.subject_input.is_focused() || self.body_area.is_focused()
    }

    /// Unfocus all text inputs. Call when focus moves away from the staging panel.
    pub fn unfocus_all(&mut self) {
        self.subject_input.set_focused(false);
        self.body_area.set_focused(false);
        self.staged_list.set_focused(false);
        self.unstaged_list.set_focused(false);
    }

    /// Update cursor blink state for text inputs. Call once per frame.
    pub fn update_cursors(&mut self, now: std::time::Instant) {
        self.subject_input.update_cursor(now);
        self.body_area.update_cursor(now);
    }

    /// Sync button styles based on current state. Call before layout.
    pub fn update_button_state(&mut self) {
        let staged_count = self.staged_list.files.len();

        // Amend toggle button styling
        if self.amend_mode {
            self.amend_btn.label = "Amend: ON".to_string();
            self.amend_btn.background = theme::STATUS_BEHIND.with_alpha(0.3);
            self.amend_btn.hover_background = theme::STATUS_BEHIND.with_alpha(0.4);
            self.amend_btn.pressed_background = theme::STATUS_BEHIND.with_alpha(0.2);
            self.amend_btn.text_color = theme::STATUS_BEHIND;
            self.amend_btn.border_color = Some(theme::STATUS_BEHIND);
        } else {
            self.amend_btn.label = "Amend".to_string();
            self.amend_btn.background = theme::SURFACE_RAISED;
            self.amend_btn.hover_background = theme::SURFACE_HOVER;
            self.amend_btn.pressed_background = theme::SURFACE;
            self.amend_btn.text_color = theme::TEXT_MUTED;
            self.amend_btn.border_color = Some(theme::BORDER);
        }

        // Commit button styling
        if self.can_commit() {
            if self.amend_mode {
                self.commit_btn.label = "Amend Commit".to_string();
                self.commit_btn.background = theme::STATUS_BEHIND;
                self.commit_btn.hover_background = crate::ui::Color::rgba(1.0, 0.70, 0.15, 1.0);
                self.commit_btn.pressed_background = crate::ui::Color::rgba(0.85, 0.50, 0.0, 1.0);
            } else {
                self.commit_btn.label = format!("Commit ({})", staged_count);
                self.commit_btn.background = theme::ACCENT;
                self.commit_btn.hover_background = crate::ui::Color::rgba(0.35, 0.70, 1.0, 1.0);
                self.commit_btn.pressed_background = crate::ui::Color::rgba(0.20, 0.55, 0.85, 1.0);
            }
            self.commit_btn.text_color = theme::TEXT_BRIGHT;
            self.commit_btn.border_color = None;
        } else {
            self.commit_btn.label = if self.amend_mode { "Amend Commit".to_string() } else { "Commit".to_string() };
            self.commit_btn.background = theme::SURFACE_RAISED;
            // No hover/press effect when disabled — looks inert
            self.commit_btn.hover_background = theme::SURFACE_RAISED;
            self.commit_btn.pressed_background = theme::SURFACE_RAISED;
            self.commit_btn.text_color = theme::TEXT_MUTED.with_alpha(0.5);
            self.commit_btn.border_color = Some(theme::BORDER.with_alpha(0.5));
        }
    }

    // ---- Worktree context switching ----

    /// Build worktree contexts from the repo's worktree list.
    /// Always populates contexts (even for 1 worktree) so name-based lookups work.
    /// The pill selector UI is only shown when there are 2+ worktrees.
    /// Preserves existing drafts by matching on path.
    pub fn set_worktrees(&mut self, worktrees: &[WorktreeInfo]) {
        if worktrees.is_empty() {
            self.worktree_contexts.clear();
            self.active_worktree_idx = 0;
            return;
        }

        let old_contexts = std::mem::take(&mut self.worktree_contexts);
        let old_active_path = old_contexts.get(self.active_worktree_idx)
            .map(|c| c.path.clone());

        let mut new_contexts: Vec<WorktreeContext> = worktrees.iter().map(|wt| {
            let path = PathBuf::from(&wt.path);
            // Preserve drafts from existing context with same path
            let (subject_draft, body_draft) = old_contexts.iter()
                .find(|c| c.path == path)
                .map(|c| (c.subject_draft.clone(), c.body_draft.clone()))
                .unwrap_or_default();

            WorktreeContext {
                name: wt.name.clone(),
                display_name: wt.name.clone(), // temporary, overwritten below
                path,
                branch: wt.branch.clone(),
                is_current: wt.is_current,
                dirty_file_count: wt.dirty_file_count,
                subject_draft,
                body_draft,
            }
        }).collect();

        // Sort: current worktree first, then alphabetical
        new_contexts.sort_by(|a, b| b.is_current.cmp(&a.is_current).then(a.name.cmp(&b.name)));

        // Compute prefix-stripped display names
        let names: Vec<String> = new_contexts.iter().map(|c| c.name.clone()).collect();
        let display_names = compute_display_names(&names);
        for (ctx, dn) in new_contexts.iter_mut().zip(display_names) {
            ctx.display_name = dn;
        }

        // Restore active index by path match, or reset to 0
        self.active_worktree_idx = if let Some(ref old_path) = old_active_path {
            new_contexts.iter().position(|c| &c.path == old_path).unwrap_or(0)
        } else {
            0
        };

        self.worktree_contexts = new_contexts;
    }

    /// Switch to a different worktree context, saving/restoring drafts.
    pub fn switch_worktree(&mut self, index: usize) {
        if index >= self.worktree_contexts.len() || index == self.active_worktree_idx {
            return;
        }

        // Save current drafts
        if self.active_worktree_idx < self.worktree_contexts.len() {
            self.worktree_contexts[self.active_worktree_idx].subject_draft =
                self.subject_input.text().to_string();
            self.worktree_contexts[self.active_worktree_idx].body_draft =
                self.body_area.text().to_string();
        }

        self.active_worktree_idx = index;

        // Restore drafts from new context
        let ctx = &self.worktree_contexts[index];
        self.subject_input.set_text(&ctx.subject_draft);
        self.body_area.set_text(&ctx.body_draft);

        // Exit amend mode on switch
        self.amend_mode = false;

        // Clear stale file lists immediately and request a fresh status refresh
        self.staged_list.set_files(Vec::new());
        self.unstaged_list.set_files(Vec::new());
        self.conflicted_list.set_files(Vec::new());
        self.status_refresh_needed = true;
    }

    /// Returns the active worktree context, if any.
    pub fn active_worktree_context(&self) -> Option<&WorktreeContext> {
        self.worktree_contexts.get(self.active_worktree_idx)
    }

    /// Whether the worktree selector pill should be shown (2+ worktrees).
    pub fn has_worktree_selector(&self) -> bool {
        self.worktree_contexts.len() >= 2
    }

    /// Number of worktree contexts.
    pub fn worktree_count(&self) -> usize {
        self.worktree_contexts.len()
    }

    /// Height of the pill bar in pixels.
    /// Shows branch name header even for single-worktree repos.
    pub fn pill_bar_height(&self, current_branch: &str) -> f32 {
        let s = self.scale;
        if self.worktree_contexts.is_empty() {
            if current_branch.is_empty() {
                0.0
            } else {
                32.0 * s
            }
        } else if self.worktree_contexts.len() == 1 {
            26.0 * s
        } else {
            let pill_h = 20.0 * s;
            let gap = 4.0 * s;
            let padding = 6.0 * s;
            self.cached_pill_rows as f32 * (pill_h + gap) + padding
        }
    }

    /// Find a worktree index by name.
    pub fn worktree_index_by_name(&self, name: &str) -> Option<usize> {
        self.worktree_contexts.iter().position(|c| c.name == name)
    }

    /// Layout the worktree pill bar at the top of the right panel.
    /// Renders wrapping rows of pills (one per worktree), active one highlighted.
    /// Single-worktree: renders just the branch name as muted text.
    pub fn layout_worktree_pills(&mut self, text_renderer: &TextRenderer, bounds: Rect, current_branch: &str) -> WidgetOutput {
        let mut output = WidgetOutput::new();
        let s = self.scale;
        self.pill_bar_rects.clear();

        if self.worktree_contexts.is_empty() {
            if !current_branch.is_empty() {
                // Single-worktree repo: show branch name as a bold header
                let bar_rect = Rect::new(bounds.x, bounds.y, bounds.width, bounds.height);
                output.spline_vertices.extend(create_rect_vertices(
                    &bar_rect,
                    theme::SURFACE_RAISED.to_array(),
                ));
                let text_y = bounds.y + (bounds.height - text_renderer.line_height()) / 2.0;
                output.text_vertices.extend(text_renderer.layout_text(
                    current_branch,
                    bounds.x + 10.0 * s,
                    text_y,
                    theme::TEXT_BRIGHT.to_array(),
                ));
            }
            return output;
        }

        if self.worktree_contexts.len() == 1 {
            // Single worktree: just show branch name as muted text
            let ctx = &self.worktree_contexts[0];
            let label = &ctx.branch;
            let text_y = bounds.y + (bounds.height - text_renderer.line_height()) / 2.0;
            output.text_vertices.extend(text_renderer.layout_text(
                label,
                bounds.x + 8.0 * s,
                text_y,
                theme::TEXT_MUTED.to_array(),
            ));

            return output;
        }

        // Multiple worktrees: render wrapping rows of pills
        let left_margin = 6.0 * s;
        let pill_h = 20.0 * s;
        let pill_gap = 4.0 * s;
        let top_pad = 3.0 * s;
        let max_x = bounds.x + bounds.width - 4.0 * s;

        let mut pill_x = bounds.x + left_margin;
        let mut row = 0;
        let mut pill_y = bounds.y + top_pad;

        for (i, ctx) in self.worktree_contexts.iter().enumerate() {
            let is_active = i == self.active_worktree_idx;
            let label = &ctx.display_name;
            let label_w = text_renderer.measure_text(label);
            // Dirty count suffix: " *N"
            let dirty_suffix = if ctx.dirty_file_count > 0 {
                format!(" *{}", ctx.dirty_file_count)
            } else {
                String::new()
            };
            let suffix_w = if !dirty_suffix.is_empty() {
                text_renderer.measure_text(&dirty_suffix)
            } else {
                0.0
            };
            let pill_w = label_w + suffix_w + 12.0 * s;

            // Wrap to next row if this pill would overflow
            if i > 0 && pill_x + pill_w > max_x {
                row += 1;
                pill_x = bounds.x + left_margin;
                pill_y = bounds.y + top_pad + row as f32 * (pill_h + pill_gap);
            }

            let pill_rect = Rect::new(pill_x, pill_y, pill_w, pill_h);
            self.pill_bar_rects.push(pill_rect);

            // Pill background
            let bg_color = if is_active {
                theme::ACCENT.with_alpha(0.2)
            } else {
                theme::SURFACE_RAISED
            };
            output.spline_vertices.extend(create_rounded_rect_vertices(
                &pill_rect,
                bg_color.to_array(),
                4.0 * s,
            ));

            // Pill border
            let border_color = if is_active {
                theme::ACCENT.with_alpha(0.5)
            } else {
                theme::BORDER
            };
            output.spline_vertices.extend(create_rounded_rect_outline_vertices(
                &pill_rect,
                border_color.to_array(),
                4.0 * s,
                1.0,
            ));

            // Pill text
            let text_color = if is_active { theme::TEXT_BRIGHT } else { theme::TEXT_MUTED };
            let text_x = pill_x + 6.0 * s;
            let text_y_pill = pill_y + (pill_h - text_renderer.line_height()) / 2.0;
            output.text_vertices.extend(text_renderer.layout_text(
                label,
                text_x,
                text_y_pill,
                text_color.to_array(),
            ));

            // Dirty count suffix in amber
            if !dirty_suffix.is_empty() {
                output.text_vertices.extend(text_renderer.layout_text(
                    &dirty_suffix,
                    text_x + label_w,
                    text_y_pill,
                    theme::STATUS_BEHIND.to_array(),
                ));
            }

            pill_x += pill_w + pill_gap;
        }

        self.cached_pill_rows = row + 1;

        output
    }

    /// Returns true if the mouse position is over a worktree pill.
    pub fn is_over_pill(&self, x: f32, y: f32) -> bool {
        self.pill_bar_rects.iter().any(|r| r.contains(x, y))
    }

    /// Get context menu items for a right-clicked worktree pill at (x, y).
    pub fn pill_context_menu_at(&self, x: f32, y: f32) -> Option<Vec<MenuItem>> {
        for (i, pill_rect) in self.pill_bar_rects.iter().enumerate() {
            if pill_rect.contains(x, y) {
                if let Some(ctx) = self.worktree_contexts.get(i) {
                    let name = &ctx.name;
                    let items = vec![
                        MenuItem::new("Switch Staging", format!("switch_worktree:{}", name)),
                        MenuItem::new("Jump to Branch", format!("jump_to_worktree:{}", name)),
                        MenuItem::new("Open in Terminal", format!("open_worktree:{}", name)),
                        MenuItem::separator(),
                        MenuItem::new("Remove Worktree", format!("remove_worktree:{}", name)),
                    ];
                    return Some(items);
                }
            }
        }
        None
    }

    /// Handle events on the worktree pill bar.
    /// Returns EventResponse::Consumed if a pill was clicked.
    pub fn handle_pill_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        if self.worktree_contexts.len() < 2 {
            return EventResponse::Ignored;
        }

        if let InputEvent::MouseDown { x, y, .. } = event {
            if !bounds.contains(*x, *y) {
                return EventResponse::Ignored;
            }

            // Check which pill was clicked using stored rects from last layout
            for (i, pill_rect) in self.pill_bar_rects.iter().enumerate() {
                if pill_rect.contains(*x, *y) {
                    self.pending_action = Some(StagingAction::SwitchWorktree(i));
                    return EventResponse::Consumed;
                }
            }
        }

        EventResponse::Ignored
    }

    /// Update hover state for child widgets based on mouse position
    pub fn update_hover(&mut self, x: f32, y: f32, bounds: Rect) {
        let regions = self.compute_regions_full(bounds);

        self.unstaged_list.update_hover(x, y, regions.unstaged);
        self.staged_list.update_hover(x, y, regions.staged);

        // Hide Stage All / Unstage All hover when the corresponding list is empty
        let zero = Rect::new(0.0, 0.0, 0.0, 0.0);
        if self.unstaged_list.files.is_empty() {
            self.stage_all_btn.update_hover(x, y, zero);
        } else {
            self.stage_all_btn.update_hover(x, y, regions.stage_all_btn);
        }
        if self.staged_list.files.is_empty() {
            self.unstage_all_btn.update_hover(x, y, zero);
        } else {
            self.unstage_all_btn.update_hover(x, y, regions.unstage_all_btn);
        }

        self.amend_btn.update_hover(x, y, regions.amend_btn);
        self.commit_btn.update_hover(x, y, regions.commit_btn);
    }

    /// Handle input events
    pub fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        // Calculate sub-regions
        let regions = self.compute_regions_full(bounds);
        let unstaged_bounds = regions.unstaged;
        let staged_bounds = regions.staged;
        let subject_bounds = regions.subject;
        let body_bounds = regions.body;

        // Tab to cycle focus within staging sections.
        // When cycling past the last section, return Ignored so Tab bubbles up
        // to the panel-level cycling in main.rs.
        if let InputEvent::KeyDown { key: Key::Tab, modifiers, .. } = event {
            let forward = !modifiers.shift;
            let sections = 4;
            if forward && self.focus_section == sections - 1 {
                // Would wrap past last section - let it bubble up
                return EventResponse::Ignored;
            } else if !forward && self.focus_section == 0 {
                // Would wrap past first section - let it bubble up
                return EventResponse::Ignored;
            }
            self.cycle_focus(forward);
            return EventResponse::Consumed;
        }

        // Ctrl+Enter to commit (or amend)
        if let InputEvent::KeyDown { key: Key::Enter, modifiers, .. } = event
            && modifiers.only_ctrl() && self.can_commit() {
                if self.amend_mode {
                    self.pending_action = Some(StagingAction::AmendCommit(self.commit_message()));
                } else {
                    self.pending_action = Some(StagingAction::Commit(self.commit_message()));
                }
                return EventResponse::Consumed;
            }

        // Handle header button clicks (Stage All, Unstage All in section headers)
        // Only process when the corresponding file list is non-empty (button is visible)
        if !self.unstaged_list.files.is_empty() {
            if self.stage_all_btn.handle_event(event, regions.stage_all_btn).is_consumed() {
                if self.stage_all_btn.was_clicked() {
                    self.pending_action = Some(StagingAction::StageAll);
                }
                return EventResponse::Consumed;
            }
        }

        if !self.staged_list.files.is_empty() {
            if self.unstage_all_btn.handle_event(event, regions.unstage_all_btn).is_consumed() {
                if self.unstage_all_btn.was_clicked() {
                    self.pending_action = Some(StagingAction::UnstageAll);
                }
                return EventResponse::Consumed;
            }
        }

        // Handle bottom button clicks (Amend, Commit)
        if self.amend_btn.handle_event(event, regions.amend_btn).is_consumed() {
            if self.amend_btn.was_clicked() {
                self.pending_action = Some(StagingAction::ToggleAmend);
            }
            return EventResponse::Consumed;
        }

        if self.commit_btn.handle_event(event, regions.commit_btn).is_consumed() {
            if self.commit_btn.was_clicked() && self.can_commit() {
                if self.amend_mode {
                    self.pending_action = Some(StagingAction::AmendCommit(self.commit_message()));
                } else {
                    self.pending_action = Some(StagingAction::Commit(self.commit_message()));
                }
            }
            return EventResponse::Consumed;
        }

        // Check clicks on submodule rows
        if let InputEvent::MouseDown { x, y, .. } = event {
            for (rect, name) in &self.submodule_bounds {
                if rect.contains(*x, *y) {
                    self.pending_action = Some(StagingAction::OpenSubmodule(name.clone()));
                    return EventResponse::Consumed;
                }
            }
            // Check clicks on sibling submodule rows
            for (rect, name) in &self.sibling_bounds {
                if rect.contains(*x, *y) {
                    self.pending_action = Some(StagingAction::SwitchToSibling(name.clone()));
                    return EventResponse::Consumed;
                }
            }
        }

        // For MouseDown, determine focus section before routing
        // New order: 0=unstaged, 1=staged, 2=subject, 3=body
        if let InputEvent::MouseDown { x, y, .. } = event {
            if unstaged_bounds.contains(*x, *y) {
                self.focus_section = 0;
                self.update_focus_state();
            } else if staged_bounds.contains(*x, *y) {
                self.focus_section = 1;
                self.update_focus_state();
            } else if subject_bounds.contains(*x, *y) {
                self.focus_section = 2;
                self.update_focus_state();
            } else if body_bounds.contains(*x, *y) {
                self.focus_section = 3;
                self.update_focus_state();
            }
        }

        // Route to focused section
        match self.focus_section {
            0 => {
                let response = self.unstaged_list.handle_event(event, unstaged_bounds);
                if response.is_consumed() {
                    if let Some(action) = self.unstaged_list.take_action() {
                        match action {
                            FileListAction::ToggleStage(path) => {
                                self.pending_action = Some(StagingAction::StageFile(path));
                            }
                            FileListAction::ViewDiff(path) => {
                                self.pending_action = Some(StagingAction::ViewDiff(path));
                            }
                            FileListAction::StageAll => {
                                self.pending_action = Some(StagingAction::StageAll);
                            }
                            FileListAction::SelectionChanged(path) => {
                                self.pending_action = Some(StagingAction::PreviewDiff(path, false));
                            }
                            _ => {}
                        }
                    }
                    return response;
                }
            }
            1 => {
                let response = self.staged_list.handle_event(event, staged_bounds);
                if response.is_consumed() {
                    if let Some(action) = self.staged_list.take_action() {
                        match action {
                            FileListAction::ToggleStage(path) => {
                                self.pending_action = Some(StagingAction::UnstageFile(path));
                            }
                            FileListAction::ViewDiff(path) => {
                                self.pending_action = Some(StagingAction::ViewDiff(path));
                            }
                            FileListAction::UnstageAll => {
                                self.pending_action = Some(StagingAction::UnstageAll);
                            }
                            FileListAction::SelectionChanged(path) => {
                                self.pending_action = Some(StagingAction::PreviewDiff(path, true));
                            }
                            _ => {}
                        }
                    }
                    return response;
                }
            }
            2 => {
                let response = self.subject_input.handle_event(event, subject_bounds);
                if response.is_consumed() {
                    return response;
                }
            }
            3 => {
                let response = self.body_area.handle_event(event, body_bounds);
                if response.is_consumed() {
                    return response;
                }
            }
            _ => {}
        }

        EventResponse::Ignored
    }

    /// Compute all regions for the new layout order.
    /// Top-to-bottom: unstaged header + list, staged header + list, commit area, buttons.
    /// When file lists are empty, they collapse to just the header row and the saved
    /// space is given to the commit message body area.
    fn compute_regions_full(&self, bounds: Rect) -> StagingRegions {
        let s = self.scale;
        let padding = 8.0 * s;
        let inner = bounds.inset(padding);

        let section_header_height = 26.0 * s;
        let divider_gap = 6.0 * s;

        let unstaged_empty = self.unstaged_list.files.is_empty();
        let staged_empty = self.staged_list.files.is_empty();

        // Bottom-up: reserve space for buttons and commit message area first
        let button_area_height = 40.0 * s;
        let commit_title_height = 22.0 * s;
        let subject_height = 32.0 * s;
        let gap_small = padding * 0.5;
        let base_body_height = 80.0 * s;

        // Submodule section: allocate a fixed height if submodules exist
        let sm_section_header_h = if self.submodules.is_empty() { 0.0 } else { 22.0 * s };
        let sm_row_h = 20.0 * s; // approximate line_height * 1.2
        let sm_max_rows = self.submodules.len().min(8) as f32;
        let sm_total_h = if self.submodules.is_empty() {
            0.0
        } else {
            divider_gap + sm_section_header_h + sm_max_rows * sm_row_h + 4.0 * s
        };

        // Sibling submodule section: allocate height if siblings exist
        let sib_section_header_h = if self.sibling_submodules.is_empty() { 0.0 } else { 22.0 * s };
        let sib_max_rows = self.sibling_submodules.len().min(8) as f32;
        let sib_total_h = if self.sibling_submodules.is_empty() {
            0.0
        } else {
            divider_gap + sib_section_header_h + sib_max_rows * sm_row_h + 4.0 * s
        };

        // File lists: when empty, collapse to zero height (header-only).
        // The header is already accounted for separately.
        let file_area_budget = (inner.height
            - section_header_height * 2.0
            - divider_gap
            - divider_gap
            - sm_total_h
            - sib_total_h
            - commit_title_height
            - subject_height
            - gap_small
            - base_body_height
            - divider_gap
            - button_area_height)
            .max(40.0 * s);

        // Distribute file area: empty lists get 0, non-empty get their share.
        let (unstaged_list_h, staged_list_h, body_bonus) = match (unstaged_empty, staged_empty) {
            (true, true) => (0.0, 0.0, file_area_budget),
            (true, false) => (0.0, file_area_budget, 0.0),
            (false, true) => (file_area_budget, 0.0, 0.0),
            (false, false) => (file_area_budget / 2.0, file_area_budget / 2.0, 0.0),
        };

        let body_height = base_body_height + body_bonus;
        let commit_total = commit_title_height + subject_height + gap_small + body_height;

        // --- Unstaged section (top) ---
        let (unstaged_header, remaining) = inner.take_top(section_header_height);
        let (unstaged, remaining) = remaining.take_top(unstaged_list_h);

        // Divider between unstaged and staged
        let (_div1, remaining) = remaining.take_top(divider_gap);

        // --- Staged section ---
        let (staged_header, remaining) = remaining.take_top(section_header_height);
        let (staged, remaining) = remaining.take_top(staged_list_h);

        // --- Submodule section (between staged and commit, only if submodules exist) ---
        let (submodules, remaining) = remaining.take_top(sm_total_h);

        // --- Sibling submodule section (after submodules, only if siblings exist) ---
        let (siblings, remaining) = remaining.take_top(sib_total_h);

        // Divider between staged/submodules/siblings and commit area
        let (_div2, remaining) = remaining.take_top(divider_gap);

        // --- Commit message area ---
        let (_commit_title_row, remaining) = remaining.take_top(commit_title_height);
        let (subject, remaining) = remaining.take_top(subject_height);
        let (_, remaining) = remaining.take_top(gap_small);
        let (body, remaining) = remaining.take_top(body_height);

        let commit_area = Rect::new(
            inner.x,
            subject.y - commit_title_height,
            inner.width,
            commit_total,
        );

        // Divider before buttons
        let (_div3, remaining) = remaining.take_top(divider_gap);

        // --- Buttons ---
        let (buttons, _) = remaining.take_top(button_area_height);

        // Stage All button in unstaged section header (right-aligned)
        let stage_all_btn_w = 80.0 * s;
        let stage_all_btn = Rect::new(
            unstaged_header.right() - stage_all_btn_w,
            unstaged_header.y + (section_header_height - 22.0 * s) / 2.0,
            stage_all_btn_w,
            22.0 * s,
        );

        // Unstage All button in staged section header (right-aligned)
        let unstage_all_btn_w = 96.0 * s;
        let unstage_all_btn = Rect::new(
            staged_header.right() - unstage_all_btn_w,
            staged_header.y + (section_header_height - 22.0 * s) / 2.0,
            unstage_all_btn_w,
            22.0 * s,
        );

        // Amend and Commit buttons in bottom button row
        let commit_btn_w = if self.amend_mode { 130.0 * s } else { 110.0 * s };
        let commit_btn = Rect::new(
            buttons.right() - commit_btn_w,
            buttons.y + 6.0 * s,
            commit_btn_w,
            28.0 * s,
        );

        let amend_btn = Rect::new(
            commit_btn.x - 98.0 * s,
            buttons.y + 6.0 * s,
            90.0 * s,
            28.0 * s,
        );

        StagingRegions {
            unstaged_header,
            unstaged,
            staged_header,
            staged,
            submodules,
            siblings,
            commit_area,
            subject,
            body,
            buttons,
            stage_all_btn,
            unstage_all_btn,
            amend_btn,
            commit_btn,
        }
    }

    /// Public region accessor for external callers.
    /// Returns (unstaged, staged, subject, body, buttons) to match the new layout order.
    pub fn compute_regions(&self, bounds: Rect) -> (Rect, Rect, Rect, Rect, Rect) {
        let r = self.compute_regions_full(bounds);
        (r.unstaged, r.staged, r.subject, r.body, r.buttons)
    }

    /// Layout the staging well
    pub fn layout(&mut self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();
        let s = self.scale;

        // Background - elevated surface for panel
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            theme::SURFACE.to_array(),
        ));

        // Border
        output.spline_vertices.extend(create_rect_outline_vertices(
            &bounds,
            theme::BORDER.to_array(),
            1.0,
        ));

        let regions = self.compute_regions_full(bounds);

        // =============================================================
        // 0. CONFLICT RESOLUTION BANNER (when merge/rebase in progress)
        // =============================================================

        let conflict_count = self.conflicted_list.files.len();
        let has_conflicts = conflict_count > 0;
        let conflict_banner_h = if has_conflicts || self.repo_state_label.is_some() { 52.0 * s } else { 0.0 };

        if has_conflicts || self.repo_state_label.is_some() {
            let banner_rect = Rect::new(bounds.x, bounds.y, bounds.width, conflict_banner_h);

            // Amber-tinted background
            output.spline_vertices.extend(create_rect_vertices(
                &banner_rect,
                [0.35, 0.25, 0.10, 0.4],
            ));

            // Banner text
            let banner_text = if has_conflicts {
                format!("Resolve {} conflict{}, then stage and commit to complete the operation",
                    conflict_count, if conflict_count == 1 { "" } else { "s" })
            } else if let Some(label) = self.repo_state_label {
                format!("{} - stage resolved files and commit", label)
            } else {
                String::new()
            };

            let amber_color = [1.0, 0.718, 0.302, 1.0]; // #FFB74D
            output.text_vertices.extend(text_renderer.layout_text(
                &banner_text,
                bounds.x + 8.0 * s,
                bounds.y + 6.0 * s,
                amber_color,
            ));

            // Show conflicted file count
            if has_conflicts {
                let conflict_label = format!("Conflicted Files ({})", conflict_count);
                output.text_vertices.extend(text_renderer.layout_text(
                    &conflict_label,
                    bounds.x + 8.0 * s,
                    bounds.y + 24.0 * s,
                    [0.937, 0.325, 0.314, 1.0], // red
                ));

                // Render conflicted file list inline (compact, just paths)
                let list_y = bounds.y + 38.0 * s;
                let conflict_bounds = Rect::new(
                    bounds.x,
                    list_y,
                    bounds.width,
                    conflict_banner_h - (list_y - bounds.y),
                );
                output.extend(self.conflicted_list.layout(text_renderer, conflict_bounds));
            }
        }

        // =============================================================
        // 1. UNSTAGED SECTION (top)
        // =============================================================

        let unstaged_count = self.unstaged_list.files.len();
        let unstaged_empty = unstaged_count == 0;

        // Section header background
        output.spline_vertices.extend(create_rect_vertices(
            &regions.unstaged_header,
            theme::STATUS_BEHIND.with_alpha(0.06).to_array(),
        ));

        // Section title
        let unstaged_title = format!("Unstaged Changes ({})", unstaged_count);
        output.text_vertices.extend(text_renderer.layout_text(
            &unstaged_title,
            regions.unstaged_header.x + 4.0 * s,
            regions.unstaged_header.y + 5.0 * s,
            if unstaged_empty { theme::TEXT_MUTED.with_alpha(0.6).to_array() } else { theme::STATUS_BEHIND.to_array() },
        ));

        if unstaged_empty {
            // Compact inline hint after header title
            let title_w = text_renderer.measure_text(&unstaged_title);
            let hint = "  \u{2014} Working tree clean";
            output.text_vertices.extend(text_renderer.layout_text(
                hint,
                regions.unstaged_header.x + 4.0 * s + title_w,
                regions.unstaged_header.y + 5.0 * s,
                theme::TEXT_MUTED.with_alpha(0.4).to_array(),
            ));
        } else {
            // Stage All button in header (only when there are files)
            output.extend(self.stage_all_btn.layout(text_renderer, regions.stage_all_btn));

            // Subtle background tint for unstaged area (orange tint)
            output.spline_vertices.extend(create_rect_vertices(
                &regions.unstaged,
                theme::STATUS_BEHIND.with_alpha(0.03).to_array(),
            ));

            // Unstaged files list
            output.extend(self.unstaged_list.layout(text_renderer, regions.unstaged));
        }

        // --- Divider between unstaged and staged ---
        // When unstaged list is empty, regions.unstaged has zero height,
        // so .bottom() == unstaged_header.bottom(), placing divider right after header.
        let div1_y = regions.unstaged.bottom() + 2.0 * s;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(bounds.x + 8.0 * s, div1_y, bounds.width - 16.0 * s, 1.0),
            theme::BORDER.to_array(),
        ));

        // =============================================================
        // 2. STAGED SECTION
        // =============================================================

        let staged_count = self.staged_list.files.len();
        let staged_empty = staged_count == 0;

        // Section header background
        output.spline_vertices.extend(create_rect_vertices(
            &regions.staged_header,
            theme::STATUS_CLEAN.with_alpha(0.06).to_array(),
        ));

        // Section title
        let staged_title = format!("Staged Changes ({})", staged_count);
        output.text_vertices.extend(text_renderer.layout_text(
            &staged_title,
            regions.staged_header.x + 4.0 * s,
            regions.staged_header.y + 5.0 * s,
            if staged_empty { theme::TEXT_MUTED.with_alpha(0.6).to_array() } else { theme::STATUS_CLEAN.to_array() },
        ));

        if staged_empty {
            // Compact inline hint after header title
            let title_w = text_renderer.measure_text(&staged_title);
            let hint = "  \u{2014} Stage files to commit them";
            output.text_vertices.extend(text_renderer.layout_text(
                hint,
                regions.staged_header.x + 4.0 * s + title_w,
                regions.staged_header.y + 5.0 * s,
                theme::TEXT_MUTED.with_alpha(0.4).to_array(),
            ));
        } else {
            // Unstage All button in header (only when there are files)
            output.extend(self.unstage_all_btn.layout(text_renderer, regions.unstage_all_btn));

            // Subtle background tint for staged area (green tint)
            output.spline_vertices.extend(create_rect_vertices(
                &regions.staged,
                theme::STATUS_CLEAN.with_alpha(0.03).to_array(),
            ));

            // Staged files list
            output.extend(self.staged_list.layout(text_renderer, regions.staged));
        }

        // --- Divider between staged and commit area ---
        let div2_y = regions.staged.bottom() + 2.0 * s;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(bounds.x + 8.0 * s, div2_y, bounds.width - 16.0 * s, 1.0),
            theme::BORDER.to_array(),
        ));

        // =============================================================
        // 3. COMMIT MESSAGE AREA
        // =============================================================

        // Subtle background for the commit area
        let commit_bg = regions.commit_area.inset(-2.0 * s);
        output.spline_vertices.extend(create_rect_vertices(
            &commit_bg,
            theme::SURFACE_RAISED.with_alpha(0.5).to_array(),
        ));
        output.spline_vertices.extend(create_rect_outline_vertices(
            &commit_bg,
            theme::BORDER.to_array(),
            1.0,
        ));

        // Title
        let title_y = regions.commit_area.y + 2.0 * s;
        let title_text = if self.amend_mode { "Amend Commit" } else { "Commit Message" };
        let title_color = if self.amend_mode { theme::STATUS_BEHIND } else { theme::TEXT_BRIGHT };
        output.text_vertices.extend(text_renderer.layout_text(
            title_text,
            regions.commit_area.x + 4.0 * s,
            title_y,
            title_color.to_array(),
        ));

        // Character count for subject - right-aligned on commit title row
        let subject_len = self.subject_input.text().chars().count();
        let char_count = format!("{}/72", subject_len);
        let count_color = if subject_len > 72 {
            theme::STATUS_DIRTY // Red when over limit
        } else if subject_len > 50 {
            theme::STATUS_BEHIND // Orange when getting close
        } else {
            theme::TEXT_MUTED
        };
        output.text_vertices.extend(text_renderer.layout_text(
            &char_count,
            regions.commit_area.right() - text_renderer.measure_text(&char_count) - 4.0 * s,
            title_y,
            count_color.to_array(),
        ));

        // Subject input
        output.extend(self.subject_input.layout(text_renderer, regions.subject));

        // Character limit progress bar below subject input
        if subject_len > 0 {
            let bar_height = 2.0;
            let bar_y = regions.subject.bottom();
            let bar_x = regions.subject.x;
            let bar_max_w = regions.subject.width;
            let ratio = (subject_len as f32 / 72.0).min(1.0);
            let bar_w = bar_max_w * ratio;
            let bar_color = if subject_len > 72 {
                theme::STATUS_DIRTY
            } else if subject_len > 50 {
                theme::STATUS_BEHIND
            } else {
                theme::STATUS_CLEAN
            };
            // Background track
            output.spline_vertices.extend(create_rect_vertices(
                &Rect::new(bar_x, bar_y, bar_max_w, bar_height),
                theme::BORDER.with_alpha(0.3).to_array(),
            ));
            // Filled portion
            output.spline_vertices.extend(create_rect_vertices(
                &Rect::new(bar_x, bar_y, bar_w, bar_height),
                bar_color.with_alpha(0.6).to_array(),
            ));
        }

        // Body area
        output.extend(self.body_area.layout(text_renderer, regions.body));

        // --- Divider between commit area and buttons ---
        let btn_divider_y = regions.buttons.y - 2.0 * s;
        output.spline_vertices.extend(create_rect_vertices(
            &Rect::new(bounds.x + 8.0 * s, btn_divider_y, bounds.width - 16.0 * s, 1.0),
            theme::BORDER.to_array(),
        ));

        // =============================================================
        // 4. BUTTON ROW (bottom)
        // =============================================================

        // Amend toggle button
        output.extend(self.amend_btn.layout(text_renderer, regions.amend_btn));

        // Commit button
        output.extend(self.commit_btn.layout(text_renderer, regions.commit_btn));

        // =============================================================
        // 5. SUBMODULES SECTION (between staged and commit, only if any exist)
        // =============================================================
        self.submodule_bounds.clear();
        if !self.submodules.is_empty() && regions.submodules.height > 0.0 {
            let sm_rect = regions.submodules;
            let sm_row_h = text_renderer.line_height() * 1.2;
            let sm_left = sm_rect.x;
            let sm_width = sm_rect.width;

            // Divider at top of submodule section
            output.spline_vertices.extend(create_rect_vertices(
                &Rect::new(sm_left, sm_rect.y, sm_width, 1.0),
                theme::BORDER.to_array(),
            ));

            // Section header
            let header_y = sm_rect.y + 4.0 * s;
            let sm_header_h = 22.0 * s;
            let sm_title = format!("Submodules ({})", self.submodules.len());
            output.text_vertices.extend(text_renderer.layout_text(
                &sm_title,
                sm_left + 4.0 * s,
                header_y + 2.0 * s,
                theme::TEXT_MUTED.to_array(),
            ));

            // Submodule rows
            let mut row_y = header_y + sm_header_h;
            let sm_bottom = sm_rect.bottom();
            for sm in &self.submodules {
                if row_y + sm_row_h > sm_bottom {
                    break;
                }

                let row_rect = Rect::new(sm_left, row_y, sm_width, sm_row_h);
                self.submodule_bounds.push((row_rect, sm.name.clone()));

                // Status dot color
                let dot_color = if sm.is_dirty {
                    theme::STATUS_BEHIND // amber
                } else if sm.branch == "detached" {
                    theme::STATUS_DIRTY // red
                } else {
                    theme::STATUS_CLEAN // green
                };

                // Status dot
                let dot = "\u{25CF}"; // ●
                let dot_x = sm_left + 4.0 * s;
                output.text_vertices.extend(text_renderer.layout_text(
                    dot,
                    dot_x,
                    row_y + 2.0,
                    dot_color.to_array(),
                ));
                let dot_w = text_renderer.measure_text(dot) + 4.0;

                // Name
                output.text_vertices.extend(text_renderer.layout_text(
                    &sm.name,
                    dot_x + dot_w,
                    row_y + 2.0,
                    theme::TEXT.to_array(),
                ));

                // Short pinned SHA on right
                if let Some(oid) = sm.head_oid {
                    let short_sha = &oid.to_string()[..7];
                    let sha_w = text_renderer.measure_text(short_sha);
                    output.text_vertices.extend(text_renderer.layout_text(
                        short_sha,
                        sm_left + sm_width - sha_w - 4.0 * s,
                        row_y + 2.0,
                        theme::TEXT_MUTED.to_array(),
                    ));
                }

                row_y += sm_row_h;
            }
        }

        // =============================================================
        // 6. SIBLINGS SECTION (sibling submodules for lateral navigation)
        // =============================================================
        self.sibling_bounds.clear();
        if !self.sibling_submodules.is_empty() && regions.siblings.height > 0.0 {
            let sib_rect = regions.siblings;
            let sib_row_h = text_renderer.line_height() * 1.2;
            let sib_left = sib_rect.x;
            let sib_width = sib_rect.width;

            // Divider at top of sibling section
            output.spline_vertices.extend(create_rect_vertices(
                &Rect::new(sib_left, sib_rect.y, sib_width, 1.0),
                theme::BORDER.to_array(),
            ));

            // Section header
            let header_y = sib_rect.y + 4.0 * s;
            let sib_header_h = 22.0 * s;
            let sib_title = format!("Siblings ({})", self.sibling_submodules.len());
            output.text_vertices.extend(text_renderer.layout_text(
                &sib_title,
                sib_left + 4.0 * s,
                header_y + 2.0 * s,
                theme::TEXT_MUTED.to_array(),
            ));

            // Sibling rows
            let mut row_y = header_y + sib_header_h;
            let sib_bottom = sib_rect.bottom();
            for sib in &self.sibling_submodules {
                if row_y + sib_row_h > sib_bottom {
                    break;
                }

                let row_rect = Rect::new(sib_left, row_y, sib_width, sib_row_h);
                self.sibling_bounds.push((row_rect, sib.name.clone()));

                // Status dot color
                let dot_color = if sib.is_dirty {
                    theme::STATUS_BEHIND // amber
                } else if sib.branch == "detached" {
                    theme::STATUS_DIRTY // red
                } else {
                    theme::STATUS_CLEAN // green
                };

                // Status dot
                let dot = "\u{25CF}"; // ●
                let dot_x = sib_left + 4.0 * s;
                output.text_vertices.extend(text_renderer.layout_text(
                    dot,
                    dot_x,
                    row_y + 2.0,
                    dot_color.to_array(),
                ));
                let dot_w = text_renderer.measure_text(dot) + 4.0;

                // Name
                output.text_vertices.extend(text_renderer.layout_text(
                    &sib.name,
                    dot_x + dot_w,
                    row_y + 2.0,
                    theme::TEXT.to_array(),
                ));

                // Short pinned SHA on right
                if let Some(oid) = sib.head_oid {
                    let short_sha = &oid.to_string()[..7];
                    let sha_w = text_renderer.measure_text(short_sha);
                    output.text_vertices.extend(text_renderer.layout_text(
                        short_sha,
                        sib_left + sib_width - sha_w - 4.0 * s,
                        row_y + 2.0,
                        theme::TEXT_MUTED.to_array(),
                    ));
                }

                row_y += sib_row_h;
            }
        }

        output
    }

}

impl Default for StagingWell {
    fn default() -> Self {
        Self::new()
    }
}
