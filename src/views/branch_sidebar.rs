//! Branch sidebar view - displays local branches, remote branches, and tags

use std::collections::HashMap;

use crate::git::{BranchTip, StashEntry, SubmoduleInfo, TagInfo, WorktreeInfo, format_relative_time};
use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{create_rect_vertices, theme, WidgetOutput};
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
    ApplyStash(usize),
    PopStash(usize),
    DropStash(usize),
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
        }
    }

    /// Update cached metrics from the text renderer (call on scale change)
    pub fn sync_metrics(&mut self, text_renderer: &TextRenderer) {
        self.line_height = text_renderer.line_height() * 1.2;
        self.section_header_height = text_renderer.line_height() * 1.6;
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
        let section_header_height = self.section_header_height;
        let section_gap = 8.0;

        let mut item_y = inner.y - self.scroll_offset;
        for (idx, item) in self.visible_items.iter().enumerate() {
            let h = match item {
                SidebarItem::SectionHeader(_) => section_header_height,
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

    /// Build the flattened visible_items list based on collapsed state
    fn build_visible_items(&mut self) {
        self.visible_items.clear();

        // LOCAL section
        self.visible_items.push(SidebarItem::SectionHeader("LOCAL"));
        if !self.local_collapsed {
            for branch in &self.local_branches {
                self.visible_items.push(SidebarItem::LocalBranch(branch.clone()));
            }
        }

        // REMOTE section
        self.visible_items.push(SidebarItem::SectionHeader("REMOTE"));
        if !self.remote_collapsed {
            let mut remote_names: Vec<&String> = self.remote_branches.keys().collect();
            remote_names.sort();
            for remote_name in remote_names {
                self.visible_items.push(SidebarItem::RemoteHeader(remote_name.clone()));
                for branch in &self.remote_branches[remote_name] {
                    self.visible_items.push(SidebarItem::RemoteBranch(remote_name.clone(), branch.clone()));
                }
            }
        }

        // TAGS section
        self.visible_items.push(SidebarItem::SectionHeader("TAGS"));
        if !self.tags_collapsed {
            for tag in &self.tags {
                self.visible_items.push(SidebarItem::Tag(tag.clone()));
            }
        }

        // SUBMODULES section (only if any exist)
        if !self.submodules.is_empty() {
            self.visible_items.push(SidebarItem::SectionHeader("SUBMODULES"));
            if !self.submodules_collapsed {
                for sm in &self.submodules {
                    self.visible_items.push(SidebarItem::SubmoduleEntry(sm.name.clone()));
                }
            }
        }

        // WORKTREES section (only if any exist)
        if !self.worktrees.is_empty() {
            self.visible_items.push(SidebarItem::SectionHeader("WORKTREES"));
            if !self.worktrees_collapsed {
                for wt in &self.worktrees {
                    self.visible_items.push(SidebarItem::WorktreeEntry(wt.name.clone()));
                }
            }
        }

        // STASHES section (only if any exist)
        if !self.stashes.is_empty() {
            self.visible_items.push(SidebarItem::SectionHeader("STASHES"));
            if !self.stashes_collapsed {
                for stash in &self.stashes {
                    self.visible_items.push(SidebarItem::StashEntry(stash.index, stash.message.clone(), stash.time));
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
        for (idx, item) in self.visible_items.iter().enumerate() {
            if idx == focused {
                break;
            }
            let h = match item {
                SidebarItem::SectionHeader(_) => self.section_header_height,
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
        let view_height = bounds.height - padding * 2.0;

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
                        self.pending_action = Some(SidebarAction::JumpToWorktreeBranch(name.clone()));
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
        let section_header_height = self.section_header_height;
        let section_gap = 8.0;

        let mut item_y = inner.y - self.scroll_offset;
        for (idx, item) in self.visible_items.iter().enumerate() {
            let h = match item {
                SidebarItem::SectionHeader(_) => section_header_height,
                _ => line_height,
            };

            if y >= item_y && y < item_y + h {
                match item {
                    SidebarItem::LocalBranch(name) => {
                        let mut items = vec![
                            MenuItem::new("Checkout", "checkout").with_shortcut("Enter"),
                            MenuItem::new("Delete Branch", "delete").with_shortcut("d"),
                            MenuItem::new("Push", "push"),
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
                            MenuItem::new("Open in Terminal", "open_worktree"),
                            MenuItem::new("Jump to Branch", "jump_to_worktree"),
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

    /// Handle input events (scrolling, clicking section headers, keyboard nav)
    pub fn handle_event(&mut self, event: &InputEvent, bounds: Rect) -> EventResponse {
        self.last_bounds = Some(bounds);

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
            InputEvent::KeyDown { key, .. } if self.focused => {
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
                    _ => {}
                }
            }
            InputEvent::MouseDown { button: MouseButton::Left, x, y, .. } => {
                if bounds.contains(*x, *y) {
                    let padding = 8.0;
                    let inner = bounds.inset(padding);
                    let line_height = self.line_height;
                    let section_header_height = self.section_header_height;
                    let section_gap = 8.0;

                    // Find which item was clicked
                    let mut item_y = inner.y - self.scroll_offset;
                    for (idx, item) in self.visible_items.iter().enumerate() {
                        let h = match item {
                            SidebarItem::SectionHeader(_) => section_header_height,
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
                                    self.pending_action = Some(SidebarAction::JumpToWorktreeBranch(name.clone()));
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

    /// Layout the sidebar and produce rendering output
    pub fn layout(&mut self, text_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
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

        let mut y = inner.y - self.scroll_offset;
        let mut item_idx: usize = 0;

        // --- LOCAL section ---
        // Section header
        y = self.layout_section_header(
            text_renderer,
            &mut output,
            &inner,
            y,
            "LOCAL",
            self.local_branches.len(),
            self.local_collapsed,
            section_header_height,
            &bounds,
        );
        item_idx += 1; // SectionHeader("LOCAL")

        if !self.local_collapsed {
            for branch in &self.local_branches {
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
                    output.text_vertices.extend(text_renderer.layout_text(
                        &display_name,
                        inner.x + indent + icon_width,
                        y + 2.0,
                        color,
                    ));
                }
                y += line_height;
                item_idx += 1;
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
            &bounds,
        );
        item_idx += 1; // SectionHeader("REMOTE")

        if !self.remote_collapsed {
            // Sort remote names for consistent order
            let mut remote_names: Vec<&String> = self.remote_branches.keys().collect();
            remote_names.sort();

            for remote_name in remote_names {
                let branches = &self.remote_branches[remote_name];

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
            &bounds,
        );
        item_idx += 1; // SectionHeader("TAGS")

        if !self.tags_collapsed {
            for tag in &self.tags {
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

        // --- SUBMODULES section (only if any exist) ---
        if !self.submodules.is_empty() {
            y += section_gap;

            y = self.layout_section_header(
                text_renderer,
                &mut output,
                &inner,
                y,
                "SUBMODULES",
                self.submodules.len(),
                self.submodules_collapsed,
                section_header_height,
                &bounds,
            );
            item_idx += 1; // SectionHeader("SUBMODULES")

            if !self.submodules_collapsed {
                for sm_name in self.submodules.iter().map(|s| s.name.clone()).collect::<Vec<_>>() {
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
        if !self.worktrees.is_empty() {
            y += section_gap;

            y = self.layout_section_header(
                text_renderer,
                &mut output,
                &inner,
                y,
                "WORKTREES",
                self.worktrees.len(),
                self.worktrees_collapsed,
                section_header_height,
                &bounds,
            );
            item_idx += 1; // SectionHeader("WORKTREES")

            if !self.worktrees_collapsed {
                for wt_name in self.worktrees.iter().map(|w| w.name.clone()).collect::<Vec<_>>() {
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

                        let display_name = truncate_to_width(&wt_name, text_renderer, inner.width - indent - icon_width);
                        output.text_vertices.extend(text_renderer.layout_text(
                            &display_name,
                            inner.x + indent + icon_width,
                            y + 2.0,
                            name_color,
                        ));
                    }
                    y += line_height;
                    item_idx += 1;
                }
            }
        }

        // --- STASHES section (only if any exist) ---
        if !self.stashes.is_empty() {
            y += section_gap;

            let stash_entries: Vec<(usize, String, i64)> = self.stashes.iter()
                .map(|s| (s.index, s.message.clone(), s.time))
                .collect();

            y = self.layout_section_header(
                text_renderer,
                &mut output,
                &inner,
                y,
                "STASHES",
                stash_entries.len(),
                self.stashes_collapsed,
                section_header_height,
                &bounds,
            );
            item_idx += 1; // SectionHeader("STASHES")

            if !self.stashes_collapsed {
                for (stash_index, stash_msg, stash_time) in &stash_entries {
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
        let mut total_h: f32 = 0.0;
        // LOCAL section
        total_h += section_header_height;
        if !self.local_collapsed {
            total_h += self.local_branches.len() as f32 * line_height;
        }
        total_h += section_gap;
        // REMOTE section
        total_h += section_header_height;
        if !self.remote_collapsed {
            for branches in self.remote_branches.values() {
                total_h += line_height; // remote name sub-header
                total_h += branches.len() as f32 * line_height;
            }
        }
        total_h += section_gap;
        // TAGS section
        total_h += section_header_height;
        if !self.tags_collapsed {
            total_h += self.tags.len() as f32 * line_height;
        }
        // SUBMODULES section
        if !self.submodules.is_empty() {
            total_h += section_gap;
            total_h += section_header_height;
            if !self.submodules_collapsed {
                total_h += self.submodules.len() as f32 * line_height;
            }
        }
        // WORKTREES section
        if !self.worktrees.is_empty() {
            total_h += section_gap;
            total_h += section_header_height;
            if !self.worktrees_collapsed {
                total_h += self.worktrees.len() as f32 * line_height;
            }
        }
        // STASHES section
        if !self.stashes.is_empty() {
            total_h += section_gap;
            total_h += section_header_height;
            if !self.stashes_collapsed {
                total_h += self.stashes.len() as f32 * line_height;
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
    #[allow(clippy::too_many_arguments)]
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
        bounds: &Rect,
    ) -> f32 {
        let visible = y + header_height >= bounds.y && y < bounds.bottom();
        if visible {
            // Section header background
            let header_rect = Rect::new(inner.x, y, inner.width, header_height);
            output.spline_vertices.extend(create_rect_vertices(
                &header_rect,
                theme::SURFACE_RAISED.to_array(),
            ));

            // Collapse indicator - Unicode triangle
            let indicator = if collapsed { "\u{25B8}" } else { "\u{25BE}" }; // ▸ / ▾
            output.text_vertices.extend(text_renderer.layout_text(
                indicator,
                inner.x + 4.0,
                y + 4.0,
                theme::TEXT.to_array(),
            ));

            // Section title - brighter than before
            output.text_vertices.extend(text_renderer.layout_text(
                title,
                inner.x + 16.0,
                y + 4.0,
                theme::TEXT_BRIGHT.to_array(),
            ));

            // Count badge
            let count_text = format!("{}", count);
            let title_width = text_renderer.measure_text(title);
            output.text_vertices.extend(text_renderer.layout_text(
                &count_text,
                inner.x + 16.0 + title_width + 8.0,
                y + 4.0,
                theme::TEXT_MUTED.to_array(),
            ));
        }

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
