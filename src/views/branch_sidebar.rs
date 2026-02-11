//! Branch sidebar view - displays local branches, remote branches, and tags

use std::collections::HashMap;

use crate::git::{BranchTip, StashEntry, SubmoduleInfo, TagInfo, WorktreeInfo, format_relative_time};
use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{create_rect_vertices, create_rect_outline_vertices, create_rounded_rect_vertices, theme, WidgetOutput};
use crate::ui::widgets::context_menu::MenuItem;
use crate::ui::widgets::scrollbar::{Scrollbar, ScrollAction};
use crate::ui::{Rect, TextRenderer};

/// Actions that can be triggered from the sidebar
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum SidebarAction {
    Checkout(String),
    CheckoutRemote(String, String), // (remote, branch)
    Delete(String),
    DeleteSubmodule(String),
    UpdateSubmodule(String),
    OpenSubmoduleTerminal(String),
    JumpToWorktreeBranch(String),
    RemoveWorktree(String),
    OpenWorktreeTerminal(String),
    SwitchWorktree(String),
    ApplyStash(usize),
    PopStash(usize),
    DropStash(usize),
    OpenSubmodule(String),
    AddRemote,
    EditRemoteUrl(String),
    RenameRemote(String),
    DeleteRemote(String),
}

/// Represents a single navigable item in the flattened sidebar list
#[allow(dead_code)]
#[derive(Clone, Debug)]
enum SidebarItem {
    SectionHeader(&'static str),
    LocalBranch(String),
    RemoteHeader(String),         // remote name like "origin"
    RemoteBranch(String, String), // (remote, branch)
    Tag(String),
    SubmoduleEntry(String),       // submodule name
    WorktreeEntry(String),        // worktree name
    StashEntry(usize, String, i64),  // (index, message, time)
}

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
    /// Submodules in the repository
    pub submodules: Vec<SubmoduleInfo>,
    /// Worktrees in the repository
    pub worktrees: Vec<WorktreeInfo>,
    /// Stash entries
    pub stashes: Vec<StashEntry>,
    /// Whether the LOCAL section is collapsed
    pub local_collapsed: bool,
    /// Whether the REMOTE section is collapsed
    pub remote_collapsed: bool,
    /// Whether the TAGS section is collapsed
    pub tags_collapsed: bool,
    /// Whether the SUBMODULES section is collapsed
    pub submodules_collapsed: bool,
    /// Whether the WORKTREES section is collapsed
    pub worktrees_collapsed: bool,
    /// Whether the STASHES section is collapsed
    pub stashes_collapsed: bool,
    /// Scroll offset for the sidebar content
    pub scroll_offset: f32,
    /// Total content height (tracked during layout)
    pub content_height: f32,
    /// Cached line height (from text renderer)
    line_height: f32,
    /// Cached section header height
    section_header_height: f32,
    /// Pending action to be consumed by main
    pending_action: Option<SidebarAction>,
    /// Whether the sidebar has keyboard focus
    pub focused: bool,
    /// Index of the focused/highlighted item in visible_items
    focused_index: Option<usize>,
    /// Flattened list of visible items (rebuilt during layout)
    visible_items: Vec<SidebarItem>,
    /// Index of the item under the mouse cursor
    hovered_index: Option<usize>,
    /// Scrollbar widget
    scrollbar: Scrollbar,
    /// Cached bounds for scroll-to-focused calculations
    last_bounds: Option<Rect>,
    /// Filter query text for searching branches/tags/stashes
    filter_query: String,
    /// Cursor position in the filter query
    filter_cursor: usize,
    /// Whether the filter input is focused (active)
    filter_focused: bool,
    /// Guard against double-insertion from KeyDown + TextInput in filter
    filter_inserted_from_key: bool,
    /// Whether the filter cursor is currently visible (for blinking)
    filter_cursor_visible: bool,
    /// Last time the filter cursor blink state changed
    filter_last_blink: std::time::Instant,
}

impl BranchSidebar {
    pub fn new() -> Self {
        Self {
            local_branches: Vec::new(),
            remote_branches: HashMap::new(),
            tags: Vec::new(),
            current_branch: String::new(),
            submodules: Vec::new(),
            worktrees: Vec::new(),
            stashes: Vec::new(),
            local_collapsed: false,
            remote_collapsed: false,
            tags_collapsed: false,
            submodules_collapsed: false,
            worktrees_collapsed: false,
            stashes_collapsed: false,
            scroll_offset: 0.0,
            content_height: 0.0,
            line_height: 18.0,
            section_header_height: 24.0,
            pending_action: None,
            focused: false,
            focused_index: None,
            visible_items: Vec::new(),
            hovered_index: None,
            scrollbar: Scrollbar::new(),
            last_bounds: None,
            filter_query: String::new(),
            filter_cursor: 0,
            filter_focused: false,
            filter_inserted_from_key: false,
            filter_cursor_visible: true,
            filter_last_blink: std::time::Instant::now(),
        }
    }

    /// Update cached metrics from the text renderer (call on scale change)
    pub fn sync_metrics(&mut self, text_renderer: &TextRenderer) {
        self.line_height = text_renderer.line_height() * 1.2;
        self.section_header_height = text_renderer.line_height() * 1.3;
    }

    /// Update filter cursor blink state. Call once per frame.
    pub fn update_filter_cursor(&mut self, now: std::time::Instant) {
        if self.filter_focused {
            if now.duration_since(self.filter_last_blink).as_millis() >= 530 {
                self.filter_cursor_visible = !self.filter_cursor_visible;
                self.filter_last_blink = now;
            }
        } else {
            self.filter_cursor_visible = true;
            self.filter_last_blink = now;
        }
    }

    /// Check if a name matches the current filter query (case-insensitive substring)
    fn matches_filter(&self, name: &str) -> bool {
        if self.filter_query.is_empty() {
            return true;
        }
        name.to_lowercase().contains(&self.filter_query.to_lowercase())
    }

    /// Total height consumed by a section header including padding.
    /// Must match `layout_section_header()`: top_pad(2) + header_height + bottom_pad(2).
    fn section_header_total_height(&self) -> f32 {
        self.section_header_height + 4.0
    }

    /// Height of the filter input bar including padding
    fn filter_bar_height(&self) -> f32 {
        self.section_header_height + 8.0 // same as section header height plus some gap
    }

    /// Returns true if a clickable sidebar item is currently hovered
    pub fn is_item_hovered(&self) -> bool {
        self.hovered_index.is_some()
    }

    /// Returns true if the given position is over the filter bar input area
    pub fn is_over_filter_bar(&self, x: f32, y: f32, bounds: Rect) -> bool {
        let fb = self.filter_bar_bounds(&bounds);
        fb.contains(x, y)
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

    /// Take the pending action (consume it)
    pub fn take_action(&mut self) -> Option<SidebarAction> {
        self.pending_action.take()
    }

    /// Set focus state
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        if focused && self.focused_index.is_none() && !self.visible_items.is_empty() {
            // Find the first branch item (skip section headers)
            self.focused_index = self.visible_items.iter().position(|item| {
                !matches!(item, SidebarItem::SectionHeader(_))
            });
        }
    }

    /// Update hover state based on mouse position
    pub fn update_hover(&mut self, x: f32, y: f32, bounds: Rect) {
        // Update scrollbar hover
        let scrollbar_width = 8.0;
        let (_content_bounds, scrollbar_bounds) = bounds.take_right(scrollbar_width);
        let move_event = crate::input::InputEvent::MouseMove {
            x, y,
            modifiers: crate::input::Modifiers::empty(),
        };
        self.scrollbar.handle_event(&move_event, scrollbar_bounds);

        if !bounds.contains(x, y) {
            self.hovered_index = None;
            return;
        }

        let padding = 8.0;
        let inner = bounds.inset(padding);
        let line_height = self.line_height;
        let section_header_total = self.section_header_total_height();
        let section_gap = 8.0;
        let filter_offset = self.filter_bar_height();

        let mut item_y = inner.y + filter_offset - self.scroll_offset;
        for (idx, item) in self.visible_items.iter().enumerate() {
            let h = match item {
                SidebarItem::SectionHeader(_) => section_header_total,
                _ => line_height,
            };

            if y >= item_y && y < item_y + h {
                // Only highlight navigable items, not section headers
                match item {
                    SidebarItem::SectionHeader(_) => {
                        self.hovered_index = None;
                    }
                    _ => {
                        self.hovered_index = Some(idx);
                    }
                }
                return;
            }

            item_y += h;

            // Section gaps
            if !matches!(item, SidebarItem::SectionHeader(_))
                && idx + 1 < self.visible_items.len()
                    && matches!(&self.visible_items[idx + 1], SidebarItem::SectionHeader(_)) {
                        item_y += section_gap;
                    }
        }

        self.hovered_index = None;
    }

    /// Build the flattened visible_items list based on collapsed state and filter
    fn build_visible_items(&mut self) {
        self.visible_items.clear();
        let filtering = !self.filter_query.is_empty();

        // LOCAL section
        let local_filtered: Vec<String> = if filtering {
            self.local_branches.iter().filter(|b| self.matches_filter(b)).cloned().collect()
        } else {
            self.local_branches.clone()
        };
        if !filtering || !local_filtered.is_empty() {
            self.visible_items.push(SidebarItem::SectionHeader("LOCAL"));
            if !self.local_collapsed {
                for branch in &local_filtered {
                    self.visible_items.push(SidebarItem::LocalBranch(branch.clone()));
                }
            }
        }

        // REMOTE section
        let mut remote_names: Vec<&String> = self.remote_branches.keys().collect();
        remote_names.sort();
        let mut remote_has_matches = false;
        let mut remote_filtered: Vec<(String, Vec<String>)> = Vec::new();
        for remote_name in &remote_names {
            let branches: Vec<String> = if filtering {
                self.remote_branches[*remote_name].iter()
                    .filter(|b| self.matches_filter(b) || self.matches_filter(remote_name))
                    .cloned().collect()
            } else {
                self.remote_branches[*remote_name].clone()
            };
            if !branches.is_empty() || !filtering {
                remote_has_matches = true;
                remote_filtered.push(((*remote_name).clone(), branches));
            }
        }
        if !filtering || remote_has_matches {
            self.visible_items.push(SidebarItem::SectionHeader("REMOTE"));
            if !self.remote_collapsed {
                for (remote_name, branches) in &remote_filtered {
                    self.visible_items.push(SidebarItem::RemoteHeader(remote_name.clone()));
                    for branch in branches {
                        self.visible_items.push(SidebarItem::RemoteBranch(remote_name.clone(), branch.clone()));
                    }
                }
            }
        }

        // TAGS section
        let tags_filtered: Vec<String> = if filtering {
            self.tags.iter().filter(|t| self.matches_filter(t)).cloned().collect()
        } else {
            self.tags.clone()
        };
        if !filtering || !tags_filtered.is_empty() {
            self.visible_items.push(SidebarItem::SectionHeader("TAGS"));
            if !self.tags_collapsed {
                for tag in &tags_filtered {
                    self.visible_items.push(SidebarItem::Tag(tag.clone()));
                }
            }
        }

        // SUBMODULES section (only if any exist)
        if !self.submodules.is_empty() {
            let sm_filtered: Vec<SubmoduleInfo> = if filtering {
                self.submodules.iter().filter(|s| self.matches_filter(&s.name)).cloned().collect()
            } else {
                self.submodules.clone()
            };
            if !filtering || !sm_filtered.is_empty() {
                self.visible_items.push(SidebarItem::SectionHeader("SUBMODULES"));
                if !self.submodules_collapsed {
                    for sm in &sm_filtered {
                        self.visible_items.push(SidebarItem::SubmoduleEntry(sm.name.clone()));
                    }
                }
            }
        }

        // WORKTREES section (only if any exist)
        if !self.worktrees.is_empty() {
            let wt_filtered: Vec<WorktreeInfo> = if filtering {
                self.worktrees.iter().filter(|w| self.matches_filter(&w.name)).cloned().collect()
            } else {
                self.worktrees.clone()
            };
            if !filtering || !wt_filtered.is_empty() {
                self.visible_items.push(SidebarItem::SectionHeader("WORKTREES"));
                if !self.worktrees_collapsed {
                    for wt in &wt_filtered {
                        self.visible_items.push(SidebarItem::WorktreeEntry(wt.name.clone()));
                    }
                }
            }
        }

        // STASHES section (only if any exist)
        if !self.stashes.is_empty() {
            let stash_filtered: Vec<StashEntry> = if filtering {
                self.stashes.iter().filter(|s| self.matches_filter(&s.message)).cloned().collect()
            } else {
                self.stashes.clone()
            };
            if !filtering || !stash_filtered.is_empty() {
                self.visible_items.push(SidebarItem::SectionHeader("STASHES"));
                if !self.stashes_collapsed {
                    for stash in &stash_filtered {
                        self.visible_items.push(SidebarItem::StashEntry(stash.index, stash.message.clone(), stash.time));
                    }
                }
            }
        }
    }

    /// Move focus to next/previous navigable item
    fn move_focus(&mut self, delta: i32) {
        if self.visible_items.is_empty() { return; }

        let current = self.focused_index.unwrap_or(0);
        let len = self.visible_items.len();

        // Search in the given direction for a navigable item (skip section headers)
        let mut idx = current as i32 + delta;
        while idx >= 0 && (idx as usize) < len {
            if matches!(&self.visible_items[idx as usize], SidebarItem::SectionHeader(_)) {
                // Skip section headers, continue searching
                idx += delta;
            } else {
                self.focused_index = Some(idx as usize);
                self.ensure_focus_visible();
                return;
            }
        }
    }

    /// Adjust scroll_offset so the focused item is visible within bounds
    fn ensure_focus_visible(&mut self) {
        let Some(focused) = self.focused_index else { return };
        let Some(bounds) = self.last_bounds else { return };

        let padding = 8.0;
        let section_gap = 8.0;

        // Compute the Y offset of the focused item relative to the content start
        let mut y_offset: f32 = 0.0;
        let section_header_total = self.section_header_total_height();
        for (idx, item) in self.visible_items.iter().enumerate() {
            if idx == focused {
                break;
            }
            let h = match item {
                SidebarItem::SectionHeader(_) => section_header_total,
                _ => self.line_height,
            };
            y_offset += h;
            // Section gaps
            if !matches!(item, SidebarItem::SectionHeader(_))
                && idx + 1 < self.visible_items.len()
                && matches!(&self.visible_items[idx + 1], SidebarItem::SectionHeader(_))
            {
                y_offset += section_gap;
            }
        }

        let item_h = self.line_height;
        let view_height = bounds.height - padding * 2.0 - self.filter_bar_height();

        // If focused item is above the visible area, scroll up
        if y_offset < self.scroll_offset {
            self.scroll_offset = y_offset;
        }
        // If focused item is below the visible area, scroll down
        else if y_offset + item_h > self.scroll_offset + view_height {
            self.scroll_offset = y_offset + item_h - view_height;
        }

        // Clamp scroll
        let max_scroll = (self.content_height - bounds.height).max(0.0);
        self.scroll_offset = self.scroll_offset.clamp(0.0, max_scroll);
    }

    /// Activate the currently focused item (checkout or toggle)
    fn activate_focused(&mut self) {
        if let Some(idx) = self.focused_index
            && let Some(item) = self.visible_items.get(idx) {
                match item {
                    SidebarItem::LocalBranch(name) => {
                        self.pending_action = Some(SidebarAction::Checkout(name.clone()));
                    }
                    SidebarItem::RemoteBranch(remote, branch) => {
                        self.pending_action = Some(SidebarAction::CheckoutRemote(remote.clone(), branch.clone()));
                    }
                    SidebarItem::WorktreeEntry(name) => {
                        self.pending_action = Some(SidebarAction::SwitchWorktree(name.clone()));
                    }
                    SidebarItem::SubmoduleEntry(name) => {
                        self.pending_action = Some(SidebarAction::OpenSubmodule(name.clone()));
                    }
                    _ => {}
                }
            }
    }

    /// Delete the currently focused branch (only local branches)
    fn delete_focused(&mut self) {
        if let Some(idx) = self.focused_index
            && let Some(SidebarItem::LocalBranch(name)) = self.visible_items.get(idx) {
                self.pending_action = Some(SidebarAction::Delete(name.clone()));
            }
    }

    /// Get context menu items for the branch at (x, y), if any.
    /// Returns (items, sidebar_action_context) describing what was right-clicked.
    pub fn context_menu_items_at(&self, x: f32, y: f32, bounds: Rect) -> Option<Vec<MenuItem>> {
        if !bounds.contains(x, y) {
            return None;
        }

        let padding = 8.0;
        let inner = bounds.inset(padding);
        let line_height = self.line_height;
        let section_header_total = self.section_header_total_height();
        let section_gap = 8.0;
        let filter_offset = self.filter_bar_height();

        let mut item_y = inner.y + filter_offset - self.scroll_offset;
        for (idx, item) in self.visible_items.iter().enumerate() {
            let h = match item {
                SidebarItem::SectionHeader(_) => section_header_total,
                _ => line_height,
            };

            if y >= item_y && y < item_y + h {
                match item {
                    SidebarItem::LocalBranch(name) => {
                        let mut items = vec![
                            MenuItem::new("Checkout", "checkout").with_shortcut("Enter"),
                            MenuItem::new("Delete Branch", "delete").with_shortcut("d"),
                            MenuItem::new("Push", "push"),
                            MenuItem::new("Pull", "pull"),
                            MenuItem::new("Pull (Rebase)", "pull_rebase"),
                            MenuItem::new("Force Push", "force_push"),
                            MenuItem::separator(),
                            MenuItem::new("Merge into Current", "merge"),
                            MenuItem::new("Rebase Current onto", "rebase"),
                        ];
                        // Tag the action_id with the branch name using a separator
                        // We'll parse it in main.rs: "checkout:branch_name"
                        for item in &mut items {
                            if !item.is_separator {
                                item.action_id = format!("{}:{}", item.action_id, name);
                            }
                        }
                        return Some(items);
                    }
                    SidebarItem::RemoteHeader(name) => {
                        let mut items = vec![
                            MenuItem::new("Edit URL...", "edit_remote_url"),
                            MenuItem::new("Rename...", "rename_remote"),
                            MenuItem::separator(),
                            MenuItem::new("Delete Remote", "delete_remote"),
                        ];
                        for item in &mut items {
                            if !item.is_separator {
                                item.action_id = format!("{}:{}", item.action_id, name);
                            }
                        }
                        return Some(items);
                    }
                    SidebarItem::RemoteBranch(remote, branch) => {
                        let full = format!("{}/{}", remote, branch);
                        let mut items = vec![
                            MenuItem::new("Checkout", "checkout_remote").with_shortcut("Enter"),
                        ];
                        for item in &mut items {
                            item.action_id = format!("{}:{}", item.action_id, full);
                        }
                        return Some(items);
                    }
                    SidebarItem::SubmoduleEntry(name) => {
                        let mut items = vec![
                            MenuItem::new("Open in Terminal", "open_submodule"),
                            MenuItem::new("Update Submodule", "update_submodule"),
                            MenuItem::separator(),
                            MenuItem::new("Delete Submodule", "delete_submodule"),
                        ];
                        for item in &mut items {
                            if !item.is_separator {
                                item.action_id = format!("{}:{}", item.action_id, name);
                            }
                        }
                        return Some(items);
                    }
                    SidebarItem::WorktreeEntry(name) => {
                        let mut items = vec![
                            MenuItem::new("Switch Staging", "switch_worktree"),
                            MenuItem::new("Jump to Branch", "jump_to_worktree"),
                            MenuItem::new("Open in Terminal", "open_worktree"),
                            MenuItem::separator(),
                            MenuItem::new("Remove Worktree", "remove_worktree"),
                        ];
                        for item in &mut items {
                            if !item.is_separator {
                                item.action_id = format!("{}:{}", item.action_id, name);
                            }
                        }
                        return Some(items);
                    }
                    SidebarItem::Tag(name) => {
                        let mut items = vec![
                            MenuItem::new("Delete Tag", "delete_tag"),
                        ];
                        for item in &mut items {
                            if !item.is_separator {
                                item.action_id = format!("{}:{}", item.action_id, name);
                            }
                        }
                        return Some(items);
                    }
                    SidebarItem::StashEntry(index, _, _) => {
                        let idx_str = index.to_string();
                        let mut items = vec![
                            MenuItem::new("Apply Stash", "apply_stash"),
                            MenuItem::new("Pop Stash", "pop_stash"),
                            MenuItem::separator(),
                            MenuItem::new("Drop Stash", "drop_stash"),
                        ];
                        for item in &mut items {
                            if !item.is_separator {
                                item.action_id = format!("{}:{}", item.action_id, idx_str);
                            }
                        }
                        return Some(items);
                    }
                    SidebarItem::SectionHeader("REMOTE") => {
                        let items = vec![
                            MenuItem::new("Add Remote...", "add_remote"),
                        ];
                        return Some(items);
                    }
                    _ => return None,
                }
            }

            item_y += h;
            if !matches!(item, SidebarItem::SectionHeader(_))
                && idx + 1 < self.visible_items.len()
                    && matches!(&self.visible_items[idx + 1], SidebarItem::SectionHeader(_)) {
                        item_y += section_gap;
                    }
        }

        None
    }

    /// Compute the filter bar bounds within the sidebar
    fn filter_bar_bounds(&self, bounds: &Rect) -> Rect {
        let padding = 8.0;
        let filter_h = self.filter_bar_height();
        Rect::new(
            bounds.x + padding,
            bounds.y + padding,
            bounds.width - padding * 2.0 - 8.0, // 8.0 for scrollbar
            filter_h - 4.0, // leave a small gap below
        )
    }

    /// Handle filter input events. Returns true if consumed.
    fn handle_filter_event(&mut self, event: &InputEvent, filter_bounds: Rect) -> EventResponse {
        match event {
            InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } => {
                if filter_bounds.contains(*x, *y) {
                    self.filter_focused = true;
                    // Position cursor based on click
                    return EventResponse::Consumed;
                } else {
                    // Click outside filter - defocus it
                    self.filter_focused = false;
                }
            }
            InputEvent::KeyDown { key, modifiers, text } if self.filter_focused => {
                match key {
                    Key::Escape => {
                        // Clear filter and defocus
                        self.filter_query.clear();
                        self.filter_cursor = 0;
                        self.filter_focused = false;
                        self.build_visible_items();
                        return EventResponse::Consumed;
                    }
                    Key::Backspace => {
                        if self.filter_cursor > 0 {
                            self.filter_cursor -= 1;
                            self.filter_query.remove(self.filter_cursor);
                            self.build_visible_items();
                        }
                        self.filter_cursor_visible = true;
                        self.filter_last_blink = std::time::Instant::now();
                        return EventResponse::Consumed;
                    }
                    Key::Delete => {
                        if self.filter_cursor < self.filter_query.len() {
                            self.filter_query.remove(self.filter_cursor);
                            self.build_visible_items();
                        }
                        self.filter_cursor_visible = true;
                        self.filter_last_blink = std::time::Instant::now();
                        return EventResponse::Consumed;
                    }
                    Key::Left => {
                        self.filter_cursor = self.filter_cursor.saturating_sub(1);
                        return EventResponse::Consumed;
                    }
                    Key::Right => {
                        self.filter_cursor = (self.filter_cursor + 1).min(self.filter_query.len());
                        return EventResponse::Consumed;
                    }
                    Key::Home => {
                        self.filter_cursor = 0;
                        return EventResponse::Consumed;
                    }
                    Key::End => {
                        self.filter_cursor = self.filter_query.len();
                        return EventResponse::Consumed;
                    }
                    Key::A if modifiers.only_ctrl() => {
                        self.filter_cursor = self.filter_query.len();
                        return EventResponse::Consumed;
                    }
                    Key::Down => {
                        // Move focus from filter to first item
                        self.filter_focused = false;
                        if !self.visible_items.is_empty() {
                            self.focused_index = self.visible_items.iter().position(|item| {
                                !matches!(item, SidebarItem::SectionHeader(_))
                            });
                        }
                        return EventResponse::Consumed;
                    }
                    Key::Enter => {
                        // Move focus from filter to first item
                        self.filter_focused = false;
                        if !self.visible_items.is_empty() {
                            self.focused_index = self.visible_items.iter().position(|item| {
                                !matches!(item, SidebarItem::SectionHeader(_))
                            });
                        }
                        return EventResponse::Consumed;
                    }
                    _ if key.is_printable() && !modifiers.ctrl && !modifiers.alt => {
                        if let Some(t) = text {
                            for c in t.chars() {
                                if !c.is_control() {
                                    self.filter_query.insert(self.filter_cursor, c);
                                    self.filter_cursor += 1;
                                }
                            }
                            self.filter_inserted_from_key = true;
                            self.filter_cursor_visible = true;
                            self.filter_last_blink = std::time::Instant::now();
                            self.build_visible_items();
                            return EventResponse::Consumed;
                        }
                    }
                    _ => {}
                }
            }
            InputEvent::TextInput(text) if self.filter_focused => {
                if self.filter_inserted_from_key {
                    self.filter_inserted_from_key = false;
                    return EventResponse::Consumed;
                }
                for c in text.chars() {
                    if !c.is_control() {
                        self.filter_query.insert(self.filter_cursor, c);
                        self.filter_cursor += 1;
                    }
                }
                if !text.is_empty() {
                    self.filter_cursor_visible = true;
                    self.filter_last_blink = std::time::Instant::now();
                    self.build_visible_items();
                    return EventResponse::Consumed;
                }
            }
            _ => {}
        }
        EventResponse::Ignored
    }

    /// Handle input events (scrolling, clicking section headers, keyboard nav)
    pub fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        self.last_bounds = Some(bounds);

        // Compute filter bar bounds
        let filter_bounds = self.filter_bar_bounds(&bounds);

        // Route filter events first
        if self.handle_filter_event(event, filter_bounds).is_consumed() {
            return EventResponse::Consumed;
        }

        // Scrollbar on right edge
        let scrollbar_width = 8.0;
        let (_content_bounds, scrollbar_bounds) = bounds.take_right(scrollbar_width);

        // Route to scrollbar first
        if self.scrollbar.handle_event(event, scrollbar_bounds).is_consumed() {
            if let Some(ScrollAction::ScrollTo(ratio)) = self.scrollbar.take_action() {
                let max_scroll = (self.content_height - bounds.height).max(0.0);
                self.scroll_offset = (ratio * max_scroll).clamp(0.0, max_scroll);
            }
            return EventResponse::Consumed;
        }

        match event {
            InputEvent::Scroll { delta_y, x, y, .. } => {
                if bounds.contains(*x, *y) {
                    self.scroll_offset = (self.scroll_offset - delta_y * 2.0)
                        .max(0.0)
                        .min((self.content_height - bounds.height).max(0.0));
                    return EventResponse::Consumed;
                }
            }
            InputEvent::KeyDown { key, modifiers, .. } if self.focused && !self.filter_focused => {
                match key {
                    Key::J | Key::Down => {
                        self.move_focus(1);
                        return EventResponse::Consumed;
                    }
                    Key::K | Key::Up => {
                        self.move_focus(-1);
                        return EventResponse::Consumed;
                    }
                    Key::Enter => {
                        self.activate_focused();
                        return EventResponse::Consumed;
                    }
                    Key::D => {
                        self.delete_focused();
                        return EventResponse::Consumed;
                    }
                    Key::PageDown => {
                        let visible_count = if self.line_height > 0.0 {
                            (bounds.height / self.line_height).max(1.0) as i32
                        } else {
                            10
                        };
                        self.move_focus(visible_count);
                        return EventResponse::Consumed;
                    }
                    Key::PageUp => {
                        let visible_count = if self.line_height > 0.0 {
                            (bounds.height / self.line_height).max(1.0) as i32
                        } else {
                            10
                        };
                        self.move_focus(-visible_count);
                        return EventResponse::Consumed;
                    }
                    // Slash key activates the filter
                    Key::Slash => {
                        self.filter_focused = true;
                        return EventResponse::Consumed;
                    }
                    // Ctrl+F also activates filter
                    Key::F if modifiers.only_ctrl() => {
                        self.filter_focused = true;
                        return EventResponse::Consumed;
                    }
                    _ => {}
                }
            }
            InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } => {
                if bounds.contains(*x, *y) {
                    let padding = 8.0;
                    let inner = bounds.inset(padding);
                    let line_height = self.line_height;
                    let section_header_total = self.section_header_total_height();
                    let section_gap = 8.0;

                    // Offset start Y by filter bar height
                    let filter_offset = self.filter_bar_height();
                    let mut item_y = inner.y + filter_offset - self.scroll_offset;
                    for (idx, item) in self.visible_items.iter().enumerate() {
                        let h = match item {
                            SidebarItem::SectionHeader(_) => section_header_total,
                            _ => line_height,
                        };

                        if *y >= item_y && *y < item_y + h {
                            match item {
                                SidebarItem::SectionHeader(name) => {
                                    // Toggle collapse
                                    match *name {
                                        "LOCAL" => self.local_collapsed = !self.local_collapsed,
                                        "REMOTE" => self.remote_collapsed = !self.remote_collapsed,
                                        "TAGS" => self.tags_collapsed = !self.tags_collapsed,
                                        "SUBMODULES" => self.submodules_collapsed = !self.submodules_collapsed,
                                        "WORKTREES" => self.worktrees_collapsed = !self.worktrees_collapsed,
                                        "STASHES" => self.stashes_collapsed = !self.stashes_collapsed,
                                        _ => {}
                                    }
                                    self.build_visible_items();
                                    return EventResponse::Consumed;
                                }
                                SidebarItem::WorktreeEntry(name) => {
                                    self.focused_index = Some(idx);
                                    self.pending_action = Some(SidebarAction::SwitchWorktree(name.clone()));
                                    return EventResponse::Consumed;
                                }
                                _ => {
                                    self.focused_index = Some(idx);
                                    return EventResponse::Consumed;
                                }
                            }
                        }

                        item_y += h;

                        // Add section gaps after the last item before the next section header
                        // We detect this by checking if the next item is a section header
                        if let SidebarItem::SectionHeader(_) = item {
                            // No extra gap right after header
                        } else if idx + 1 < self.visible_items.len()
                            && let SidebarItem::SectionHeader(_) = &self.visible_items[idx + 1] {
                                item_y += section_gap;
                            }
                    }

                    return EventResponse::Consumed;
                }
            }
            _ => {}
        }
        EventResponse::Ignored
    }

    /// Layout the sidebar and produce rendering output.
    /// `bold_renderer` is used for section headers (LOCAL, REMOTE, etc.).
    pub fn layout(&mut self, text_renderer: &TextRenderer, bold_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();
        self.last_bounds = Some(bounds);

        self.build_visible_items();

        // Panel background - slightly darker for depth
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            theme::PANEL_SIDEBAR.to_array(),
        ));

        let padding = 8.0;
        let inner = bounds.inset(padding);
        let line_height = self.line_height;
        let section_header_height = self.section_header_height;
        let indent = 12.0;
        let section_gap = 8.0;
        let filter_bar_h = self.filter_bar_height();

        // --- Filter bar at the top ---
        {
            let fb = self.filter_bar_bounds(&bounds);

            // Filter background
            let corner_radius = 4.0;
            output.spline_vertices.extend(create_rounded_rect_vertices(
                &fb,
                theme::SURFACE.to_array(),
                corner_radius,
            ));

            // Border
            let border_color = if self.filter_focused {
                theme::ACCENT
            } else {
                theme::BORDER
            };
            let border_thickness = if self.filter_focused { 2.0 } else { 1.0 };
            output.spline_vertices.extend(create_rect_outline_vertices(
                &fb,
                border_color.to_array(),
                border_thickness,
            ));

            let text_y = fb.y + (fb.height - text_renderer.line_height()) / 2.0;
            let text_x = fb.x + 8.0;

            // Search icon
            let icon = "\u{25CB}"; // ○ as search icon placeholder
            output.text_vertices.extend(text_renderer.layout_text(
                icon,
                text_x,
                text_y,
                theme::TEXT_MUTED.to_array(),
            ));
            let icon_width = text_renderer.measure_text(icon) + 4.0;
            let input_x = text_x + icon_width;

            if self.filter_query.is_empty() {
                output.text_vertices.extend(text_renderer.layout_text(
                    "Filter branches...",
                    input_x,
                    text_y,
                    theme::TEXT_MUTED.with_alpha(0.5).to_array(),
                ));
            } else {
                output.text_vertices.extend(text_renderer.layout_text(
                    &self.filter_query,
                    input_x,
                    text_y,
                    theme::TEXT_BRIGHT.to_array(),
                ));

                // Clear "x" on right side
                let clear_text = "x";
                let clear_width = text_renderer.measure_text(clear_text);
                output.text_vertices.extend(text_renderer.layout_text(
                    clear_text,
                    fb.right() - clear_width - 8.0,
                    text_y,
                    theme::TEXT_MUTED.to_array(),
                ));
            }

            // Cursor
            if self.filter_focused && self.filter_cursor_visible {
                let cursor_x = input_x + text_renderer.measure_text(&self.filter_query[..self.filter_cursor]);
                let cursor_rect = Rect::new(cursor_x, fb.y + 4.0, 2.0, fb.height - 8.0);
                output.spline_vertices.extend(create_rect_vertices(
                    &cursor_rect,
                    theme::ACCENT.to_array(),
                ));
            }
        }

        // Pre-compute filtered data for rendering
        let filtering = !self.filter_query.is_empty();
        let filtered_local: Vec<String> = if filtering {
            self.local_branches.iter().filter(|b| self.matches_filter(b)).cloned().collect()
        } else {
            self.local_branches.clone()
        };

        let mut remote_names_sorted: Vec<String> = self.remote_branches.keys().cloned().collect();
        remote_names_sorted.sort();
        let filtered_remotes: Vec<(String, Vec<String>)> = remote_names_sorted.iter().filter_map(|rn| {
            let branches: Vec<String> = if filtering {
                self.remote_branches[rn].iter()
                    .filter(|b| self.matches_filter(b) || self.matches_filter(rn))
                    .cloned().collect()
            } else {
                self.remote_branches[rn].clone()
            };
            if !branches.is_empty() || !filtering {
                Some((rn.clone(), branches))
            } else {
                None
            }
        }).collect();

        let filtered_tags: Vec<String> = if filtering {
            self.tags.iter().filter(|t| self.matches_filter(t)).cloned().collect()
        } else {
            self.tags.clone()
        };

        let filtered_submodules: Vec<String> = if filtering {
            self.submodules.iter().filter(|s| self.matches_filter(&s.name)).map(|s| s.name.clone()).collect()
        } else {
            self.submodules.iter().map(|s| s.name.clone()).collect()
        };

        let filtered_worktrees: Vec<String> = if filtering {
            self.worktrees.iter().filter(|w| self.matches_filter(&w.name)).map(|w| w.name.clone()).collect()
        } else {
            self.worktrees.iter().map(|w| w.name.clone()).collect()
        };

        let filtered_stashes: Vec<(usize, String, i64)> = if filtering {
            self.stashes.iter().filter(|s| self.matches_filter(&s.message)).map(|s| (s.index, s.message.clone(), s.time)).collect()
        } else {
            self.stashes.iter().map(|s| (s.index, s.message.clone(), s.time)).collect()
        };

        let show_local = !filtering || !filtered_local.is_empty();
        let show_remote = !filtering || !filtered_remotes.is_empty();
        let show_tags = !filtering || !filtered_tags.is_empty();
        let show_submodules = !self.submodules.is_empty() && (!filtering || !filtered_submodules.is_empty());
        let show_worktrees = !self.worktrees.is_empty() && (!filtering || !filtered_worktrees.is_empty());
        let show_stashes = !self.stashes.is_empty() && (!filtering || !filtered_stashes.is_empty());

        let mut y = inner.y + filter_bar_h - self.scroll_offset;
        let mut item_idx: usize = 0;

        // --- LOCAL section ---
        if show_local {
        // Section header
        y = self.layout_section_header(
            text_renderer,
            bold_renderer,
            &mut output,
            &inner,
            y,
            "LOCAL",
            filtered_local.len(),
            self.local_collapsed,
            section_header_height,
            &bounds,
        );
        item_idx += 1; // SectionHeader("LOCAL")

        if !self.local_collapsed {
            for branch in &filtered_local {
                let visible = y + line_height >= bounds.y && y < bounds.bottom();
                if visible {
                    let is_current = *branch == self.current_branch;
                    let is_focused = self.focused && self.focused_index == Some(item_idx);
                    let is_hovered = self.hovered_index == Some(item_idx);

                    // Hover highlight (drawn first, lowest layer)
                    if is_hovered && !is_focused {
                        let highlight_rect = Rect::new(
                            inner.x,
                            y,
                            inner.width,
                            line_height,
                        );
                        output.spline_vertices.extend(create_rect_vertices(
                            &highlight_rect,
                            theme::SURFACE_HOVER.with_alpha(0.3).to_array(),
                        ));
                    }

                    // Focus highlight (drawn before current-branch highlight so current still shows)
                    if is_focused {
                        let highlight_rect = Rect::new(
                            inner.x,
                            y,
                            inner.width,
                            line_height,
                        );
                        output.spline_vertices.extend(create_rect_vertices(
                            &highlight_rect,
                            theme::SURFACE_HOVER.to_array(),
                        ));
                    }

                    if is_current {
                        // Accent left stripe for current branch
                        let stripe_rect = Rect::new(
                            inner.x,
                            y,
                            3.0,
                            line_height,
                        );
                        output.spline_vertices.extend(create_rect_vertices(
                            &stripe_rect,
                            theme::ACCENT.to_array(),
                        ));
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
                    } else if is_hovered || is_focused {
                        theme::TEXT_BRIGHT.to_array()
                    } else {
                        theme::TEXT.to_array()
                    };

                    // Branch icon prefix
                    let icon = if is_current { "\u{25CF}" } else { "\u{25CB}" }; // ● / ○
                    let icon_color = if is_current {
                        theme::ACCENT.to_array()
                    } else {
                        theme::TEXT_MUTED.to_array()
                    };
                    output.text_vertices.extend(text_renderer.layout_text(
                        icon,
                        inner.x + indent,
                        y + 2.0,
                        icon_color,
                    ));
                    let icon_width = text_renderer.measure_text(icon) + 4.0;

                    let display_name = truncate_to_width(branch, text_renderer, inner.width - indent - icon_width);
                    if is_current {
                        // Active branch in bold
                        output.bold_text_vertices.extend(bold_renderer.layout_text(
                            &display_name,
                            inner.x + indent + icon_width,
                            y + 2.0,
                            color,
                        ));
                    } else {
                        output.text_vertices.extend(text_renderer.layout_text(
                            &display_name,
                            inner.x + indent + icon_width,
                            y + 2.0,
                            color,
                        ));
                    }
                }
                y += line_height;
                item_idx += 1;
            }
        }

        y += section_gap;
        } // end show_local

        // --- REMOTE section ---
        if show_remote {
        let remote_count: usize = filtered_remotes.iter().map(|(_, v)| v.len()).sum();
        y = self.layout_section_header(
            text_renderer,
            bold_renderer,
            &mut output,
            &inner,
            y,
            "REMOTE",
            remote_count,
            self.remote_collapsed,
            section_header_height,
            &bounds,
        );
        item_idx += 1; // SectionHeader("REMOTE")

        if !self.remote_collapsed {
            for (remote_name, branches) in &filtered_remotes {

                // Remote name sub-header
                let visible = y + line_height >= bounds.y && y < bounds.bottom();
                if visible {
                    let is_focused = self.focused && self.focused_index == Some(item_idx);
                    let is_hovered = self.hovered_index == Some(item_idx);
                    if is_hovered && !is_focused {
                        let highlight_rect = Rect::new(inner.x, y, inner.width, line_height);
                        output.spline_vertices.extend(create_rect_vertices(
                            &highlight_rect,
                            theme::SURFACE_HOVER.with_alpha(0.3).to_array(),
                        ));
                    }
                    if is_focused {
                        let highlight_rect = Rect::new(inner.x, y, inner.width, line_height);
                        output.spline_vertices.extend(create_rect_vertices(
                            &highlight_rect,
                            theme::SURFACE_HOVER.to_array(),
                        ));
                    }
                    let remote_color = if is_hovered || is_focused {
                        theme::TEXT.to_array()
                    } else {
                        theme::TEXT_MUTED.to_array()
                    };
                    // Remote icon prefix
                    let remote_label = format!("\u{2601} {}", remote_name); // ☁ icon
                    output.text_vertices.extend(text_renderer.layout_text(
                        &remote_label,
                        inner.x + indent,
                        y + 2.0,
                        remote_color,
                    ));
                }
                y += line_height;
                item_idx += 1; // RemoteHeader

                // Branches under this remote
                for branch in branches {
                    let visible = y + line_height >= bounds.y && y < bounds.bottom();
                    if visible {
                        let is_focused = self.focused && self.focused_index == Some(item_idx);
                        let is_hovered = self.hovered_index == Some(item_idx);
                        if is_hovered && !is_focused {
                            let highlight_rect = Rect::new(inner.x, y, inner.width, line_height);
                            output.spline_vertices.extend(create_rect_vertices(
                                &highlight_rect,
                                theme::SURFACE_HOVER.with_alpha(0.3).to_array(),
                            ));
                        }
                        if is_focused {
                            let highlight_rect = Rect::new(inner.x, y, inner.width, line_height);
                            output.spline_vertices.extend(create_rect_vertices(
                                &highlight_rect,
                                theme::SURFACE_HOVER.to_array(),
                            ));
                        }
                        let branch_color = if is_hovered || is_focused {
                            theme::TEXT_BRIGHT.to_array()
                        } else {
                            theme::BRANCH_REMOTE.to_array()
                        };
                        // Remote branch icon
                        output.text_vertices.extend(text_renderer.layout_text(
                            "\u{25CB}", // ○
                            inner.x + indent * 2.0,
                            y + 2.0,
                            theme::TEXT_MUTED.to_array(),
                        ));
                        let icon_width = text_renderer.measure_text("\u{25CB}") + 4.0;
                        let display_name = truncate_to_width(branch, text_renderer, inner.width - indent * 2.0 - icon_width);
                        output.text_vertices.extend(text_renderer.layout_text(
                            &display_name,
                            inner.x + indent * 2.0 + icon_width,
                            y + 2.0,
                            branch_color,
                        ));
                    }
                    y += line_height;
                    item_idx += 1;
                }
            }
        }

        y += section_gap;
        } // end show_remote

        // --- TAGS section ---
        if show_tags {
        y = self.layout_section_header(
            text_renderer,
            bold_renderer,
            &mut output,
            &inner,
            y,
            "TAGS",
            filtered_tags.len(),
            self.tags_collapsed,
            section_header_height,
            &bounds,
        );
        item_idx += 1; // SectionHeader("TAGS")

        if !self.tags_collapsed {
            for tag in &filtered_tags {
                let visible = y + line_height >= bounds.y && y < bounds.bottom();
                if visible {
                    let is_focused = self.focused && self.focused_index == Some(item_idx);
                    let is_hovered = self.hovered_index == Some(item_idx);
                    if is_hovered && !is_focused {
                        let highlight_rect = Rect::new(inner.x, y, inner.width, line_height);
                        output.spline_vertices.extend(create_rect_vertices(
                            &highlight_rect,
                            theme::SURFACE_HOVER.with_alpha(0.3).to_array(),
                        ));
                    }
                    if is_focused {
                        let highlight_rect = Rect::new(inner.x, y, inner.width, line_height);
                        output.spline_vertices.extend(create_rect_vertices(
                            &highlight_rect,
                            theme::SURFACE_HOVER.to_array(),
                        ));
                    }
                    let tag_color = if is_hovered || is_focused {
                        theme::TEXT_BRIGHT.to_array()
                    } else {
                        theme::BRANCH_RELEASE.to_array()
                    };
                    // Tag icon prefix
                    output.text_vertices.extend(text_renderer.layout_text(
                        "\u{2691}", // ⚑
                        inner.x + indent,
                        y + 2.0,
                        theme::BRANCH_RELEASE.to_array(),
                    ));
                    let icon_width = text_renderer.measure_text("\u{2691}") + 4.0;
                    let display_name = truncate_to_width(tag, text_renderer, inner.width - indent - icon_width);
                    output.text_vertices.extend(text_renderer.layout_text(
                        &display_name,
                        inner.x + indent + icon_width,
                        y + 2.0,
                        tag_color,
                    ));
                }
                y += line_height;
                item_idx += 1;
            }
        }

        } // end show_tags

        // --- SUBMODULES section (only if any exist) ---
        if show_submodules {
            y += section_gap;

            y = self.layout_section_header(
                text_renderer,
                bold_renderer,
                &mut output,
                &inner,
                y,
                "SUBMODULES",
                filtered_submodules.len(),
                self.submodules_collapsed,
                section_header_height,
                &bounds,
            );
            item_idx += 1; // SectionHeader("SUBMODULES")

            if !self.submodules_collapsed {
                for sm_name in &filtered_submodules {
                    let sm_name = sm_name.clone();
                    let visible = y + line_height >= bounds.y && y < bounds.bottom();
                    if visible {
                        let is_focused = self.focused && self.focused_index == Some(item_idx);
                        let is_hovered = self.hovered_index == Some(item_idx);
                        if is_hovered && !is_focused {
                            let highlight_rect = Rect::new(inner.x, y, inner.width, line_height);
                            output.spline_vertices.extend(create_rect_vertices(
                                &highlight_rect,
                                theme::SURFACE_HOVER.with_alpha(0.3).to_array(),
                            ));
                        }
                        if is_focused {
                            let highlight_rect = Rect::new(inner.x, y, inner.width, line_height);
                            output.spline_vertices.extend(create_rect_vertices(
                                &highlight_rect,
                                theme::SURFACE_HOVER.to_array(),
                            ));
                        }
                        let name_color = if is_hovered || is_focused {
                            theme::TEXT_BRIGHT.to_array()
                        } else {
                            theme::TEXT.to_array()
                        };

                        // Submodule icon: ■ in green
                        let icon = "\u{25A0}"; // ■
                        output.text_vertices.extend(text_renderer.layout_text(
                            icon,
                            inner.x + indent,
                            y + 2.0,
                            theme::BRANCH_FEATURE.to_array(),
                        ));
                        let icon_width = text_renderer.measure_text(icon) + 4.0;

                        // Check dirty status
                        let is_dirty = self.submodules.iter()
                            .any(|s| s.name == sm_name && s.is_dirty);

                        // Show dirty indicator after name if dirty
                        let dirty_marker = " \u{25CF}M"; // ●M
                        let suffix_width = if is_dirty {
                            text_renderer.measure_text(dirty_marker)
                        } else {
                            0.0
                        };

                        let display_name = truncate_to_width(&sm_name, text_renderer, inner.width - indent - icon_width - suffix_width);
                        output.text_vertices.extend(text_renderer.layout_text(
                            &display_name,
                            inner.x + indent + icon_width,
                            y + 2.0,
                            name_color,
                        ));

                        if is_dirty {
                            let name_width = text_renderer.measure_text(&display_name);
                            output.text_vertices.extend(text_renderer.layout_text(
                                dirty_marker,
                                inner.x + indent + icon_width + name_width,
                                y + 2.0,
                                theme::STATUS_DIRTY.to_array(),
                            ));
                        }
                    }
                    y += line_height;
                    item_idx += 1;
                }
            }
        }

        // --- WORKTREES section (only if any exist) ---
        if show_worktrees {
            y += section_gap;

            y = self.layout_section_header(
                text_renderer,
                bold_renderer,
                &mut output,
                &inner,
                y,
                "WORKTREES",
                filtered_worktrees.len(),
                self.worktrees_collapsed,
                section_header_height,
                &bounds,
            );
            item_idx += 1; // SectionHeader("WORKTREES")

            if !self.worktrees_collapsed {
                for wt_name in &filtered_worktrees {
                    let wt_name = wt_name.clone();
                    let visible = y + line_height >= bounds.y && y < bounds.bottom();
                    if visible {
                        let is_focused = self.focused && self.focused_index == Some(item_idx);
                        let is_hovered = self.hovered_index == Some(item_idx);

                        let is_current = self.worktrees.iter()
                            .any(|w| w.name == wt_name && w.is_current);

                        if is_hovered && !is_focused {
                            let highlight_rect = Rect::new(inner.x, y, inner.width, line_height);
                            output.spline_vertices.extend(create_rect_vertices(
                                &highlight_rect,
                                theme::SURFACE_HOVER.with_alpha(0.3).to_array(),
                            ));
                        }
                        if is_focused {
                            let highlight_rect = Rect::new(inner.x, y, inner.width, line_height);
                            output.spline_vertices.extend(create_rect_vertices(
                                &highlight_rect,
                                theme::SURFACE_HOVER.to_array(),
                            ));
                        }

                        // Accent highlight for current worktree (like current branch)
                        if is_current {
                            let stripe_rect = Rect::new(inner.x, y, 3.0, line_height);
                            output.spline_vertices.extend(create_rect_vertices(
                                &stripe_rect,
                                theme::ACCENT.to_array(),
                            ));
                            let highlight_rect = Rect::new(inner.x, y, inner.width, line_height);
                            output.spline_vertices.extend(create_rect_vertices(
                                &highlight_rect,
                                theme::ACCENT_MUTED.to_array(),
                            ));
                        }

                        let name_color = if is_current {
                            theme::ACCENT.to_array()
                        } else if is_hovered || is_focused {
                            theme::TEXT_BRIGHT.to_array()
                        } else {
                            theme::TEXT.to_array()
                        };

                        // Worktree icon: ▣ in orange
                        let icon = "\u{25A3}"; // ▣
                        let icon_color = if is_current {
                            theme::ACCENT.to_array()
                        } else {
                            theme::BRANCH_RELEASE.to_array()
                        };
                        output.text_vertices.extend(text_renderer.layout_text(
                            icon,
                            inner.x + indent,
                            y + 2.0,
                            icon_color,
                        ));
                        let icon_width = text_renderer.measure_text(icon) + 4.0;

                        // Check dirty status
                        let is_dirty = self.worktrees.iter()
                            .any(|w| w.name == wt_name && w.is_dirty);

                        // Show dirty indicator after name if dirty
                        let dirty_marker = " \u{25CF}M"; // ●M
                        let suffix_width = if is_dirty {
                            text_renderer.measure_text(dirty_marker)
                        } else {
                            0.0
                        };

                        let display_name = truncate_to_width(&wt_name, text_renderer, inner.width - indent - icon_width - suffix_width);
                        output.text_vertices.extend(text_renderer.layout_text(
                            &display_name,
                            inner.x + indent + icon_width,
                            y + 2.0,
                            name_color,
                        ));

                        if is_dirty {
                            let name_width = text_renderer.measure_text(&display_name);
                            output.text_vertices.extend(text_renderer.layout_text(
                                dirty_marker,
                                inner.x + indent + icon_width + name_width,
                                y + 2.0,
                                theme::STATUS_DIRTY.to_array(),
                            ));
                        }
                    }
                    y += line_height;
                    item_idx += 1;
                }
            }
        }

        // --- STASHES section (only if any exist) ---
        if show_stashes {
            y += section_gap;

            y = self.layout_section_header(
                text_renderer,
                bold_renderer,
                &mut output,
                &inner,
                y,
                "STASHES",
                filtered_stashes.len(),
                self.stashes_collapsed,
                section_header_height,
                &bounds,
            );
            item_idx += 1; // SectionHeader("STASHES")

            if !self.stashes_collapsed {
                for (stash_index, stash_msg, stash_time) in &filtered_stashes {
                    let visible = y + line_height >= bounds.y && y < bounds.bottom();
                    if visible {
                        let is_focused = self.focused && self.focused_index == Some(item_idx);
                        let is_hovered = self.hovered_index == Some(item_idx);
                        if is_hovered && !is_focused {
                            let highlight_rect = Rect::new(inner.x, y, inner.width, line_height);
                            output.spline_vertices.extend(create_rect_vertices(
                                &highlight_rect,
                                theme::SURFACE_HOVER.with_alpha(0.3).to_array(),
                            ));
                        }
                        if is_focused {
                            let highlight_rect = Rect::new(inner.x, y, inner.width, line_height);
                            output.spline_vertices.extend(create_rect_vertices(
                                &highlight_rect,
                                theme::SURFACE_HOVER.to_array(),
                            ));
                        }
                        let name_color = if is_hovered || is_focused {
                            theme::TEXT_BRIGHT.to_array()
                        } else {
                            theme::TEXT.to_array()
                        };

                        // Stash icon: ⬒ in a muted color
                        let icon = "\u{25C8}"; // ◈
                        output.text_vertices.extend(text_renderer.layout_text(
                            icon,
                            inner.x + indent,
                            y + 2.0,
                            theme::TEXT_MUTED.to_array(),
                        ));
                        let icon_width = text_renderer.measure_text(icon) + 4.0;

                        // Show relative time on the right side
                        let time_str = if *stash_time > 0 {
                            format_relative_time(*stash_time)
                        } else {
                            String::new()
                        };
                        let time_width = if !time_str.is_empty() {
                            text_renderer.measure_text(&time_str) + 8.0
                        } else {
                            0.0
                        };

                        // Display the stash message (e.g. "stash@{0}: WIP on main")
                        let label = format!("@{{{}}}: {}", stash_index,
                            stash_msg.split(": ").skip(1).collect::<Vec<_>>().join(": ")
                                .chars().take(60).collect::<String>()
                        );
                        let display_name = truncate_to_width(&label, text_renderer, inner.width - indent - icon_width - time_width);
                        output.text_vertices.extend(text_renderer.layout_text(
                            &display_name,
                            inner.x + indent + icon_width,
                            y + 2.0,
                            name_color,
                        ));

                        // Right-aligned time
                        if !time_str.is_empty() {
                            let time_x = inner.right() - text_renderer.measure_text(&time_str);
                            output.text_vertices.extend(text_renderer.layout_text(
                                &time_str,
                                time_x,
                                y + 2.0,
                                theme::TEXT_MUTED.to_array(),
                            ));
                        }
                    }
                    y += line_height;
                    item_idx += 1;
                }
            }
        }

        // Compute total content height for scroll clamping (independent of early-break rendering)
        let mut total_h: f32 = filter_bar_h;
        let section_header_total = self.section_header_total_height();
        // LOCAL section
        if show_local {
            total_h += section_header_total;
            if !self.local_collapsed {
                total_h += filtered_local.len() as f32 * line_height;
            }
            total_h += section_gap;
        }
        // REMOTE section
        if show_remote {
            total_h += section_header_total;
            if !self.remote_collapsed {
                for (_, branches) in &filtered_remotes {
                    total_h += line_height; // remote name sub-header
                    total_h += branches.len() as f32 * line_height;
                }
            }
            total_h += section_gap;
        }
        // TAGS section
        if show_tags {
            total_h += section_header_total;
            if !self.tags_collapsed {
                total_h += filtered_tags.len() as f32 * line_height;
            }
        }
        // SUBMODULES section
        if show_submodules {
            total_h += section_gap;
            total_h += section_header_total;
            if !self.submodules_collapsed {
                total_h += filtered_submodules.len() as f32 * line_height;
            }
        }
        // WORKTREES section
        if show_worktrees {
            total_h += section_gap;
            total_h += section_header_total;
            if !self.worktrees_collapsed {
                total_h += filtered_worktrees.len() as f32 * line_height;
            }
        }
        // STASHES section
        if show_stashes {
            total_h += section_gap;
            total_h += section_header_total;
            if !self.stashes_collapsed {
                total_h += filtered_stashes.len() as f32 * line_height;
            }
        }
        self.content_height = total_h;

        // Update and render scrollbar
        let scrollbar_width = 8.0;
        let total_items = self.visible_items.len();
        let visible_items = (bounds.height / line_height).max(1.0) as usize;
        let scroll_offset_items = if line_height > 0.0 {
            (self.scroll_offset / line_height).round() as usize
        } else {
            0
        };
        self.scrollbar.set_content(total_items, visible_items, scroll_offset_items);
        let (_content_area, scrollbar_bounds) = bounds.take_right(scrollbar_width);
        let scrollbar_output = self.scrollbar.layout(scrollbar_bounds);
        output.spline_vertices.extend(scrollbar_output.spline_vertices);

        // Suppress unused variable warning
        let _ = item_idx;

        output
    }

    /// Layout a section header (e.g., "LOCAL  3") and return the new y position.
    /// Performs bounds checking to skip rendering if the header is outside visible area.
    /// Uses regular weight text at 85% scale with muted color for a subtle appearance.
    #[allow(clippy::too_many_arguments)]
    fn layout_section_header(
        &self,
        text_renderer: &TextRenderer,
        _bold_renderer: &TextRenderer,
        output: &mut WidgetOutput,
        inner: &Rect,
        y: f32,
        title: &str,
        count: usize,
        collapsed: bool,
        header_height: f32,
        bounds: &Rect,
    ) -> f32 {
        let top_pad = 2.0;
        let bottom_pad = 2.0;
        let inset = 6.0;
        let total_height = top_pad + header_height + bottom_pad;
        let visible = y + total_height >= bounds.y && y < bounds.bottom();
        if visible {
            let empty = count == 0;
            let header_scale = 0.85;
            let text_h = text_renderer.line_height() * header_scale;
            let text_y = y + top_pad + (header_height - text_h) * 0.5;

            // Only show background for non-empty sections
            if !empty {
                let header_rect = Rect::new(
                    inner.x + inset,
                    y + top_pad,
                    inner.width - inset * 2.0,
                    header_height,
                );
                output.spline_vertices.extend(create_rounded_rect_vertices(
                    &header_rect,
                    theme::SURFACE_RAISED.with_alpha(0.3).to_array(),
                    3.0,
                ));
            }

            // Dimmer color for empty sections
            let label_color = if empty {
                theme::TEXT_MUTED.with_alpha(0.4).to_array()
            } else {
                theme::TEXT_MUTED.to_array()
            };

            // Collapse indicator - small chevron
            let indicator = if collapsed { "\u{25B8}" } else { "\u{25BE}" }; // ▸ / ▾
            output.text_vertices.extend(text_renderer.layout_text_scaled(
                indicator,
                inner.x + inset + 4.0,
                text_y,
                label_color,
                header_scale,
            ));

            // Section title in regular weight, muted color, smaller scale
            let indicator_width = text_renderer.measure_text_scaled(indicator, header_scale) + 2.0;
            output.text_vertices.extend(text_renderer.layout_text_scaled(
                title,
                inner.x + inset + 4.0 + indicator_width,
                text_y,
                label_color,
                header_scale,
            ));

            // Count badge - even smaller, more muted
            let count_text = format!("{}", count);
            let title_width = text_renderer.measure_text_scaled(title, header_scale);
            let badge_scale = 0.75;
            let badge_h = text_renderer.line_height() * badge_scale;
            let badge_y = y + top_pad + (header_height - badge_h) * 0.5;
            let badge_color = if empty {
                theme::TEXT_MUTED.with_alpha(0.3).to_array()
            } else {
                theme::TEXT_MUTED.with_alpha(0.5).to_array()
            };
            output.text_vertices.extend(text_renderer.layout_text_scaled(
                &count_text,
                inner.x + inset + 4.0 + indicator_width + title_width + 6.0,
                badge_y,
                badge_color,
                badge_scale,
            ));
        }

        y + total_height
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
