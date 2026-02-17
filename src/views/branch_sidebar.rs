//! Branch sidebar view - displays local branches, remote branches, and tags

use std::collections::{HashMap, HashSet};

use crate::git::{BranchTip, StashEntry, TagInfo, WorktreeInfo, format_relative_time};
use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{create_rect_vertices, create_rect_outline_vertices, create_rounded_rect_vertices, theme, WidgetOutput};
use crate::ui::widgets::context_menu::MenuItem;
use crate::ui::widgets::scrollbar::{Scrollbar, ScrollAction};
use crate::ui::{Rect, SplineVertex, TextRenderer};
use crate::ui::text_util::{truncate_to_width, clamp_scroll};

/// Actions that can be triggered from the sidebar
#[derive(Clone, Debug)]
pub enum SidebarAction {
    Checkout(String),
    CheckoutRemote(String, String), // (remote, branch)
    Delete(String),
    ApplyStash(usize),
    DropStash(usize),
    DeleteTag(String),
    SwitchWorktree(String), // worktree name to switch to
}

/// Represents a single navigable item in the flattened sidebar list
#[derive(Clone, Debug)]
enum SidebarItem {
    SectionHeader(&'static str),
    LocalBranch(String),
    RemoteHeader(String),         // remote name like "origin"
    RemoteBranch(String, String), // (remote, branch)
    Tag(String),
    StashEntry(usize),  // stash index
}

/// Shared layout parameters passed to section rendering methods
struct LayoutParams<'a> {
    text_renderer: &'a TextRenderer,
    inner: Rect,
    bounds: Rect,
    line_height: f32,
    section_header_height: f32,
    indent: f32,
    /// Top Y below which items are visible (below the filter bar)
    content_top: f32,
}

/// Pre-computed filtered data for all sections
struct FilteredData {
    local: Vec<String>,
    remotes: Vec<(String, Vec<String>)>,
    tags: Vec<String>,
    stashes: Vec<(usize, String, i64)>,
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
    /// Stash entries
    pub stashes: Vec<StashEntry>,
    /// Whether the LOCAL section is collapsed
    pub local_collapsed: bool,
    /// Whether the REMOTE section is collapsed
    pub remote_collapsed: bool,
    /// Whether the TAGS section is collapsed
    pub tags_collapsed: bool,
    /// Whether the STASHES section is collapsed
    pub stashes_collapsed: bool,
    /// Per-remote collapse state (e.g., "origin" collapsed independently)
    collapsed_remotes: HashSet<String>,
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
    /// Ahead/behind counts per local branch name (only entries with non-zero values)
    ahead_behind_cache: HashMap<String, (usize, usize)>,
    /// Upstream tracking branch per local branch (e.g. "main" -> "origin/main")
    upstream_map: HashMap<String, String>,
    /// Map from branch name to worktree name (branches checked out in worktrees)
    branch_worktree_map: HashMap<String, String>,
    /// Name of the currently active worktree (if in bare repo with worktrees)
    active_worktree_name: Option<String>,
    /// Branch checked out in the active worktree
    active_worktree_branch: Option<String>,
    /// Whether the repo is effectively bare
    is_bare_repo: bool,
    /// Number of worktrees (for context menu logic)
    worktree_count: usize,
    /// Worktree paths indexed by name (for checkout_in_wt action)
    worktree_paths: HashMap<String, String>,
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

/// Create a small filled triangle chevron using spline vertices.
/// `collapsed=true` → right-pointing ▸, `collapsed=false` → down-pointing ▾.
fn create_chevron_vertices(x: f32, y: f32, size: f32, collapsed: bool, color: [f32; 4]) -> Vec<SplineVertex> {
    if collapsed {
        // Right-pointing triangle: 3 vertices
        vec![
            SplineVertex { position: [x, y], color },
            SplineVertex { position: [x, y + size], color },
            SplineVertex { position: [x + size * 0.7, y + size * 0.5], color },
        ]
    } else {
        // Down-pointing triangle: 3 vertices
        vec![
            SplineVertex { position: [x, y], color },
            SplineVertex { position: [x + size, y], color },
            SplineVertex { position: [x + size * 0.5, y + size * 0.7], color },
        ]
    }
}

impl BranchSidebar {
    pub fn new() -> Self {
        Self {
            local_branches: Vec::new(),
            remote_branches: HashMap::new(),
            tags: Vec::new(),
            current_branch: String::new(),
            stashes: Vec::new(),
            ahead_behind_cache: HashMap::new(),
            upstream_map: HashMap::new(),
            local_collapsed: false,
            remote_collapsed: false,
            tags_collapsed: false,
            stashes_collapsed: false,
            collapsed_remotes: HashSet::new(),
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
            branch_worktree_map: HashMap::new(),
            active_worktree_name: None,
            active_worktree_branch: None,
            is_bare_repo: false,
            worktree_count: 0,
            worktree_paths: HashMap::new(),
            filter_query: String::new(),
            filter_cursor: 0,
            filter_focused: false,
            filter_inserted_from_key: false,
            filter_cursor_visible: true,
            filter_last_blink: std::time::Instant::now(),
        }
    }

    /// Returns true if the filter search bar has text focus
    pub fn has_text_focus(&self) -> bool {
        self.filter_focused
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

    /// Find the index of the visible item at the given Y coordinate.
    /// Returns None if Y is in the filter bar, outside bounds, or past all items.
    fn item_index_at_y(&self, y: f32, bounds: Rect) -> Option<usize> {
        let padding = 8.0;
        let inner = bounds.inset(padding);
        let section_header_total = self.section_header_total_height();
        let section_gap = 8.0;
        let filter_offset = self.filter_bar_height();
        let content_top = bounds.y + padding + filter_offset;

        if y < content_top { return None; }

        let mut item_y = inner.y + filter_offset - self.scroll_offset;
        for (idx, item) in self.visible_items.iter().enumerate() {
            let h = match item {
                SidebarItem::SectionHeader(_) => section_header_total,
                _ => self.line_height,
            };

            if y >= item_y && y < item_y + h && item_y >= content_top {
                return Some(idx);
            }

            item_y += h;
            if !matches!(item, SidebarItem::SectionHeader(_))
                && idx + 1 < self.visible_items.len()
                && matches!(&self.visible_items[idx + 1], SidebarItem::SectionHeader(_))
            {
                item_y += section_gap;
            }
        }
        None
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
        all_remote_names: &[String],
        worktrees: &[WorktreeInfo],
        active_worktree_name: Option<&str>,
        is_bare: bool,
    ) {
        self.current_branch = current_branch;

        // Separate local and remote branches
        self.local_branches.clear();
        self.remote_branches.clear();
        self.upstream_map.clear();

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
                if let Some(upstream) = &tip.upstream {
                    self.upstream_map.insert(tip.name.clone(), upstream.clone());
                }
            }
        }

        // Ensure all configured remotes appear even if they have 0 tracking branches
        for remote_name in all_remote_names {
            self.remote_branches.entry(remote_name.clone()).or_insert_with(Vec::new);
        }

        // Sort for consistent display
        self.local_branches.sort();
        for branches in self.remote_branches.values_mut() {
            branches.sort();
        }

        self.tags = tags.iter().map(|t| t.name.clone()).collect();
        self.tags.sort();

        // Build worktree awareness data
        self.branch_worktree_map.clear();
        self.worktree_paths.clear();
        self.worktree_count = worktrees.len();
        self.is_bare_repo = is_bare;
        for wt in worktrees {
            if !wt.branch.is_empty() {
                self.branch_worktree_map.insert(wt.branch.clone(), wt.name.clone());
            }
            self.worktree_paths.insert(wt.name.clone(), wt.path.clone());
        }
        self.active_worktree_name = active_worktree_name.map(|s| s.to_string());
        self.active_worktree_branch = active_worktree_name
            .and_then(|name| worktrees.iter().find(|wt| wt.name == name))
            .map(|wt| wt.branch.clone())
            .filter(|b| !b.is_empty());
    }

    /// The branch that merge/rebase operations would target — the active
    /// worktree's branch if set, otherwise the repo's current_branch.
    pub fn effective_branch(&self) -> &str {
        self.active_worktree_branch.as_deref()
            .unwrap_or(&self.current_branch)
    }

    /// Update the ahead/behind cache for local branches
    pub fn update_ahead_behind(&mut self, data: HashMap<String, (usize, usize)>) {
        self.ahead_behind_cache = data;
    }

    /// Read-only access to the ahead/behind cache (for diagnostic snapshots)
    pub fn ahead_behind_cache(&self) -> HashMap<String, (usize, usize)> {
        self.ahead_behind_cache.clone()
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
        let scrollbar_width = theme::SCROLLBAR_WIDTH;
        let (_content_bounds, scrollbar_bounds) = bounds.take_right(scrollbar_width);
        let move_event = crate::input::InputEvent::MouseMove { x, y };
        self.scrollbar.handle_event(&move_event, scrollbar_bounds);

        if !bounds.contains(x, y) {
            self.hovered_index = None;
            return;
        }

        self.hovered_index = self.item_index_at_y(y, bounds)
            .filter(|&i| !matches!(self.visible_items[i], SidebarItem::SectionHeader(_)));
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
                    if !self.collapsed_remotes.contains(remote_name) {
                        for branch in branches {
                            self.visible_items.push(SidebarItem::RemoteBranch(remote_name.clone(), branch.clone()));
                        }
                    }
                }
            }
        }

        // TAGS section (only if any exist)
        if !self.tags.is_empty() {
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
                        self.visible_items.push(SidebarItem::StashEntry(stash.index));
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
        self.scroll_offset = clamp_scroll(self.scroll_offset, self.content_height, bounds.height);
    }

    /// Activate the currently focused item (checkout or toggle)
    fn activate_focused(&mut self) {
        if let Some(idx) = self.focused_index
            && let Some(item) = self.visible_items.get(idx) {
                match item {
                    SidebarItem::LocalBranch(name) => {
                        // If the branch is checked out in a worktree, switch to that worktree
                        if let Some(wt_name) = self.branch_worktree_map.get(name) {
                            self.pending_action = Some(SidebarAction::SwitchWorktree(wt_name.clone()));
                        } else {
                            self.pending_action = Some(SidebarAction::Checkout(name.clone()));
                        }
                    }
                    SidebarItem::RemoteBranch(remote, branch) => {
                        self.pending_action = Some(SidebarAction::CheckoutRemote(remote.clone(), branch.clone()));
                    }
                    SidebarItem::Tag(name) => {
                        self.pending_action = Some(SidebarAction::Checkout(name.clone()));
                    }
                    SidebarItem::StashEntry(index) => {
                        self.pending_action = Some(SidebarAction::ApplyStash(*index));
                    }
                    _ => {}
                }
            }
    }

    /// Delete the currently focused branch/tag or drop stash
    fn delete_focused(&mut self) {
        if let Some(idx) = self.focused_index {
            match self.visible_items.get(idx) {
                Some(SidebarItem::LocalBranch(name)) => {
                    self.pending_action = Some(SidebarAction::Delete(name.clone()));
                }
                Some(SidebarItem::Tag(name)) => {
                    self.pending_action = Some(SidebarAction::DeleteTag(name.clone()));
                }
                Some(SidebarItem::StashEntry(index)) => {
                    self.pending_action = Some(SidebarAction::DropStash(*index));
                }
                _ => {}
            }
        }
    }

    /// Get context menu items for the branch at (x, y), if any.
    /// Returns (items, sidebar_action_context) describing what was right-clicked.
    pub fn context_menu_items_at(&self, x: f32, y: f32, bounds: Rect) -> Option<Vec<MenuItem>> {
        if !bounds.contains(x, y) {
            return None;
        }

        let idx = self.item_index_at_y(y, bounds)?;
        match &self.visible_items[idx] {
            SidebarItem::LocalBranch(name) => {
                let mut items = Vec::new();

                // Checkout action depends on worktree model
                if let Some(wt_name) = self.branch_worktree_map.get(name) {
                    // Branch is already checked out in a worktree — offer switch
                    // Pre-format with worktree name (not branch name) since the
                    // tail loop would incorrectly append branch name
                    items.push(MenuItem::new(
                        format!("Switch to '{}'", wt_name),
                        &format!("switch_worktree:{}", wt_name),
                    ).with_shortcut("Enter"));
                } else if !self.is_bare_repo || self.worktree_count > 0 {
                    if self.worktree_count > 1 {
                        // Multiple worktrees — offer per-worktree checkout
                        let mut wt_names: Vec<&String> = self.worktree_paths.keys().collect();
                        wt_names.sort();
                        for wt in wt_names {
                            items.push(MenuItem::new(
                                format!("Checkout in '{}'", wt),
                                &format!("checkout_in_wt:{}|{}", name, wt),
                            ));
                        }
                    } else {
                        // Single worktree or normal repo
                        items.push(MenuItem::new("Checkout", "checkout").with_shortcut("Enter"));
                    }
                } else {
                    // True bare repo with no worktrees — only HEAD pointer
                    items.push(MenuItem::new("Set as HEAD", "set_head").with_shortcut("Enter"));
                }

                items.push(MenuItem::separator());
                items.push(MenuItem::new("Pull", "pull"));
                items.push(MenuItem::new("Pull (Rebase)", "pull_rebase"));
                items.push(MenuItem::new("Push", "push"));
                items.push(MenuItem::separator());
                items.push(MenuItem::new("Pull from...", "pull_from_dialog"));
                items.push(MenuItem::new("Push to...", "push_to"));
                items.push(MenuItem::new("Force Push", "force_push"));

                // Merge/rebase only when there's an effective branch to merge into
                let effective = self.effective_branch();
                if !effective.is_empty() && effective != name {
                    items.push(MenuItem::separator());
                    items.push(MenuItem::new(
                        format!("Merge into '{}'", effective), "merge"
                    ));
                    items.push(MenuItem::new(
                        format!("Rebase '{}' onto", effective), "rebase"
                    ));
                }

                items.push(MenuItem::separator());
                items.push(MenuItem::new("Rename...", "rename"));
                items.push(MenuItem::new("Create Worktree", "create_worktree"));
                items.push(MenuItem::new("Delete Branch", "delete").with_shortcut("d"));

                // Tag action_ids with the branch name (skip items already tagged via checkout_in_wt)
                for item in &mut items {
                    if !item.is_separator && !item.action_id.contains(':') {
                        item.action_id = format!("{}:{}", item.action_id, name);
                    }
                }
                Some(items)
            }
            SidebarItem::RemoteHeader(name) => {
                let mut items = vec![
                    MenuItem::new(format!("Fetch from {}", name), "fetch_remote"),
                    MenuItem::separator(),
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
                Some(items)
            }
            SidebarItem::RemoteBranch(remote, branch) => {
                let full = format!("{}/{}", remote, branch);
                let mut items = vec![
                    MenuItem::new("Checkout", "checkout_remote").with_shortcut("Enter"),
                ];

                // Merge/rebase only when there's an effective branch to merge into
                let effective = self.effective_branch();
                if !effective.is_empty() {
                    items.push(MenuItem::new(
                        format!("Merge into '{}'", effective), "merge_remote"
                    ));
                    items.push(MenuItem::new(
                        format!("Rebase '{}' onto", effective), "rebase_remote"
                    ));
                }

                items.push(MenuItem::separator());
                items.push(MenuItem::new("Delete Remote Branch", "delete_remote_branch"));

                for item in &mut items {
                    if !item.is_separator {
                        item.action_id = format!("{}:{}", item.action_id, full);
                    }
                }
                Some(items)
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
                Some(items)
            }
            SidebarItem::StashEntry(index) => {
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
                Some(items)
            }
            SidebarItem::SectionHeader("REMOTE") => {
                Some(vec![
                    MenuItem::new("Fetch All Remotes", "fetch_all"),
                    MenuItem::new("Add Remote...", "add_remote"),
                ])
            }
            _ => None,
        }
    }

    /// Compute the filter bar bounds within the sidebar
    fn filter_bar_bounds(&self, bounds: &Rect) -> Rect {
        let padding = 8.0;
        let filter_h = self.filter_bar_height();
        Rect::new(
            bounds.x + padding,
            bounds.y + padding,
            bounds.width - padding * 2.0 - theme::SCROLLBAR_WIDTH, // scrollbar
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
        let scrollbar_width = theme::SCROLLBAR_WIDTH;
        let (_content_bounds, scrollbar_bounds) = bounds.take_right(scrollbar_width);

        // Route to scrollbar first
        if self.scrollbar.handle_event(event, scrollbar_bounds).is_consumed() {
            if let Some(ScrollAction::ScrollTo(ratio)) = self.scrollbar.take_action() {
                let max_scroll = (self.content_height - bounds.height).max(0.0);
                self.scroll_offset = clamp_scroll(ratio * max_scroll, self.content_height, bounds.height);
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
                    if let Some(idx) = self.item_index_at_y(*y, bounds) {
                        match &self.visible_items[idx] {
                            SidebarItem::SectionHeader(name) => {
                                // Toggle collapse
                                match *name {
                                    "LOCAL" => self.local_collapsed = !self.local_collapsed,
                                    "REMOTE" => self.remote_collapsed = !self.remote_collapsed,
                                    "TAGS" => self.tags_collapsed = !self.tags_collapsed,
                                    "STASHES" => self.stashes_collapsed = !self.stashes_collapsed,
                                    _ => {}
                                }
                                self.build_visible_items();
                                return EventResponse::Consumed;
                            }
                            SidebarItem::RemoteHeader(name) => {
                                // Toggle per-remote collapse
                                if self.collapsed_remotes.contains(name) {
                                    self.collapsed_remotes.remove(name);
                                } else {
                                    self.collapsed_remotes.insert(name.clone());
                                }
                                self.build_visible_items();
                                return EventResponse::Consumed;
                            }
                            _ => {
                                self.focused_index = Some(idx);
                                return EventResponse::Consumed;
                            }
                        }
                    }

                    return EventResponse::Consumed;
                }
            }
            _ => {}
        }
        EventResponse::Ignored
    }

    /// Build the pre-computed filtered data for all sections.
    fn build_filtered_data(&self) -> FilteredData {
        let filtering = !self.filter_query.is_empty();

        let local: Vec<String> = if filtering {
            self.local_branches.iter().filter(|b| self.matches_filter(b)).cloned().collect()
        } else {
            self.local_branches.clone()
        };

        let mut remote_names_sorted: Vec<String> = self.remote_branches.keys().cloned().collect();
        remote_names_sorted.sort();
        let remotes: Vec<(String, Vec<String>)> = remote_names_sorted.iter().filter_map(|rn| {
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

        let tags: Vec<String> = if filtering {
            self.tags.iter().filter(|t| self.matches_filter(t)).cloned().collect()
        } else {
            self.tags.clone()
        };

        let stashes: Vec<(usize, String, i64)> = if filtering {
            self.stashes.iter().filter(|s| self.matches_filter(&s.message)).map(|s| (s.index, s.message.clone(), s.time)).collect()
        } else {
            self.stashes.iter().map(|s| (s.index, s.message.clone(), s.time)).collect()
        };

        FilteredData { local, remotes, tags, stashes }
    }

    /// Render hover/focus highlight backgrounds for a sidebar item row.
    /// Returns the text color to use for the item.
    fn layout_item_highlight(
        &self,
        output: &mut WidgetOutput,
        inner: &Rect,
        y: f32,
        line_height: f32,
        item_idx: usize,
        default_color: [f32; 4],
        bright_color: [f32; 4],
    ) -> [f32; 4] {
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

        if is_hovered || is_focused {
            bright_color
        } else {
            default_color
        }
    }

    /// Render the filter bar at the top of the sidebar.
    fn layout_filter_bar(&self, text_renderer: &TextRenderer, output: &mut WidgetOutput, bounds: &Rect) {
        let fb = self.filter_bar_bounds(bounds);

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

    /// Layout the LOCAL branches section. Returns (new_y, new_item_idx).
    fn layout_local_section(
        &self,
        params: &LayoutParams,
        bold_renderer: &TextRenderer,
        output: &mut WidgetOutput,
        filtered_local: &[String],
        y: f32,
        item_idx: usize,
    ) -> (f32, usize) {
        let mut y = y;
        let mut item_idx = item_idx;

        y = self.layout_section_header(
            params.text_renderer, bold_renderer, output, &params.inner, y,
            "LOCAL", filtered_local.len(), self.local_collapsed,
            params.section_header_height, &params.bounds, params.content_top,
        );
        item_idx += 1; // SectionHeader("LOCAL")

        if !self.local_collapsed {
            for branch in filtered_local {
                let visible = y >= params.content_top && y < params.bounds.bottom();
                if visible {
                    let is_current = *branch == self.current_branch
                        || self.active_worktree_branch.as_deref() == Some(branch.as_str());
                    let in_other_worktree = !is_current
                        && self.branch_worktree_map.contains_key(branch);
                    let is_focused = self.focused && self.focused_index == Some(item_idx);
                    let is_hovered = self.hovered_index == Some(item_idx);

                    // Hover highlight
                    if is_hovered && !is_focused {
                        let highlight_rect = Rect::new(params.inner.x, y, params.inner.width, params.line_height);
                        output.spline_vertices.extend(create_rect_vertices(
                            &highlight_rect,
                            theme::SURFACE_HOVER.with_alpha(0.3).to_array(),
                        ));
                    }

                    // Focus highlight
                    if is_focused {
                        let highlight_rect = Rect::new(params.inner.x, y, params.inner.width, params.line_height);
                        output.spline_vertices.extend(create_rect_vertices(
                            &highlight_rect,
                            theme::SURFACE_HOVER.to_array(),
                        ));
                    }

                    if is_current {
                        // Accent left stripe for current branch
                        let stripe_rect = Rect::new(params.inner.x, y, 3.0, params.line_height);
                        output.spline_vertices.extend(create_rect_vertices(
                            &stripe_rect,
                            theme::ACCENT.to_array(),
                        ));
                        // Highlight background for current branch
                        let highlight_rect = Rect::new(params.inner.x, y, params.inner.width, params.line_height);
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

                    // Branch icon prefix — amber dot for branches in other worktrees
                    let icon = if is_current { "\u{25CF}" } else if in_other_worktree { "\u{25CF}" } else { "\u{25CB}" }; // ● / ○
                    let icon_color = if is_current {
                        theme::ACCENT.to_array()
                    } else if in_other_worktree {
                        [1.0, 0.718, 0.302, 1.0] // amber for worktree-occupied branches
                    } else {
                        theme::TEXT_MUTED.to_array()
                    };
                    output.text_vertices.extend(params.text_renderer.layout_text(
                        icon,
                        params.inner.x + params.indent,
                        y + 2.0,
                        icon_color,
                    ));
                    let icon_width = params.text_renderer.measure_text(icon) + 4.0;

                    // Compute ahead/behind indicator width for right-alignment
                    let ab = self.ahead_behind_cache.get(branch.as_str());
                    let mut ab_total_width = 0.0f32;
                    if let Some(&(ahead, behind)) = ab {
                        if ahead > 0 {
                            let ahead_text = format!("\u{2191}{}", ahead);
                            ab_total_width += params.text_renderer.measure_text(&ahead_text) + 6.0;
                        }
                        if behind > 0 {
                            let behind_text = format!("\u{2193}{}", behind);
                            ab_total_width += params.text_renderer.measure_text(&behind_text) + 6.0;
                        }
                    }

                    // Compute upstream tracking label width
                    let upstream_scale = 0.85;
                    let upstream_label = self.upstream_map.get(branch.as_str())
                        .map(|u| format!("\u{2192} {}", u));
                    let upstream_width = upstream_label.as_ref()
                        .map(|label| params.text_renderer.measure_text_scaled(label, upstream_scale) + 6.0)
                        .unwrap_or(0.0);

                    // Compute worktree indicator width for non-active worktree branches
                    let wt_label_scale = 0.85;
                    let wt_label = if in_other_worktree {
                        self.branch_worktree_map.get(branch).map(|wt| format!("[{}]", wt))
                    } else {
                        None
                    };
                    let wt_label_width = wt_label.as_ref()
                        .map(|label| params.text_renderer.measure_text_scaled(label, wt_label_scale) + 6.0)
                        .unwrap_or(0.0);

                    let right_reserved = ab_total_width + upstream_width + wt_label_width;
                    let name_max_width = params.inner.width - params.indent - icon_width - right_reserved;
                    let display_name = truncate_to_width(branch, params.text_renderer, name_max_width);
                    if is_current {
                        // Active branch in bold
                        output.bold_text_vertices.extend(bold_renderer.layout_text(
                            &display_name,
                            params.inner.x + params.indent + icon_width,
                            y + 2.0,
                            color,
                        ));
                    } else {
                        output.text_vertices.extend(params.text_renderer.layout_text(
                            &display_name,
                            params.inner.x + params.indent + icon_width,
                            y + 2.0,
                            color,
                        ));
                    }

                    // Render ahead/behind indicators (right-aligned)
                    let mut right_x = params.inner.right();
                    if let Some(&(ahead, behind)) = ab {
                        if behind > 0 {
                            let behind_text = format!("\u{2193}{}", behind);
                            let behind_w = params.text_renderer.measure_text(&behind_text);
                            right_x -= behind_w;
                            output.text_vertices.extend(params.text_renderer.layout_text(
                                &behind_text,
                                right_x,
                                y + 2.0,
                                theme::STATUS_BEHIND.to_array(),
                            ));
                            right_x -= 6.0;
                        }
                        if ahead > 0 {
                            let ahead_text = format!("\u{2191}{}", ahead);
                            let ahead_w = params.text_renderer.measure_text(&ahead_text);
                            right_x -= ahead_w;
                            output.text_vertices.extend(params.text_renderer.layout_text(
                                &ahead_text,
                                right_x,
                                y + 2.0,
                                theme::STATUS_CLEAN.to_array(),
                            ));
                            right_x -= 6.0;
                        }
                    }

                    // Render upstream tracking label (right-aligned, after ahead/behind)
                    if let Some(label) = &upstream_label {
                        let label_w = params.text_renderer.measure_text_scaled(label, upstream_scale);
                        right_x -= label_w;
                        // Vertically center the smaller text
                        let y_offset = params.line_height * (1.0 - upstream_scale) * 0.3;
                        output.text_vertices.extend(params.text_renderer.layout_text_scaled(
                            label,
                            right_x,
                            y + 2.0 + y_offset,
                            theme::TEXT_MUTED.to_array(),
                            upstream_scale,
                        ));
                    }

                    // Render worktree indicator for branches in other worktrees
                    if let Some(wt_text) = &wt_label {
                        let wt_w = params.text_renderer.measure_text_scaled(wt_text, wt_label_scale);
                        right_x -= wt_w;
                        let y_offset = params.line_height * (1.0 - wt_label_scale) * 0.3;
                        output.text_vertices.extend(params.text_renderer.layout_text_scaled(
                            wt_text,
                            right_x,
                            y + 2.0 + y_offset,
                            [1.0, 0.718, 0.302, 0.7], // amber, slightly muted
                            wt_label_scale,
                        ));
                    }
                }
                y += params.line_height;
                item_idx += 1;
            }
        }

        (y, item_idx)
    }

    /// Layout the REMOTE branches section. Returns (new_y, new_item_idx).
    fn layout_remote_section(
        &self,
        params: &LayoutParams,
        bold_renderer: &TextRenderer,
        output: &mut WidgetOutput,
        filtered_remotes: &[(String, Vec<String>)],
        y: f32,
        item_idx: usize,
    ) -> (f32, usize) {
        let mut y = y;
        let mut item_idx = item_idx;

        let remote_count: usize = filtered_remotes.len();  // count of remotes, not branches
        y = self.layout_section_header(
            params.text_renderer, bold_renderer, output, &params.inner, y,
            "REMOTE", remote_count, self.remote_collapsed,
            params.section_header_height, &params.bounds, params.content_top,
        );
        item_idx += 1; // SectionHeader("REMOTE")

        if !self.remote_collapsed {
            for (remote_name, branches) in filtered_remotes {
                let remote_is_collapsed = self.collapsed_remotes.contains(remote_name);
                // Remote name sub-header
                let visible = y >= params.content_top && y < params.bounds.bottom();
                if visible {
                    let remote_color = self.layout_item_highlight(
                        output, &params.inner, y, params.line_height, item_idx,
                        theme::TEXT_MUTED.to_array(), theme::TEXT.to_array(),
                    );
                    // Chevron indicator for collapse state (spline triangle)
                    let chevron_size = params.line_height * 0.45;
                    let chevron_x = params.inner.x + params.indent;
                    let chevron_y = y + (params.line_height - chevron_size) * 0.5;
                    output.spline_vertices.extend(create_chevron_vertices(
                        chevron_x, chevron_y, chevron_size,
                        remote_is_collapsed,
                        theme::TEXT_MUTED.with_alpha(0.6).to_array(),
                    ));
                    let chevron_w = chevron_size + 4.0;
                    let remote_label = format!("\u{2601} {}", remote_name); // ☁ icon
                    output.text_vertices.extend(params.text_renderer.layout_text(
                        &remote_label,
                        params.inner.x + params.indent + chevron_w,
                        y + 2.0,
                        remote_color,
                    ));
                }
                y += params.line_height;
                item_idx += 1; // RemoteHeader

                // Branches under this remote (skip if collapsed)
                if !remote_is_collapsed {
                    if branches.is_empty() {
                        // Show hint for remotes with no tracking branches
                        let visible = y >= params.content_top && y < params.bounds.bottom();
                        if visible {
                            let hint = "Fetch to see branches";
                            output.text_vertices.extend(params.text_renderer.layout_text(
                                hint,
                                params.inner.x + params.indent * 2.0,
                                y + 2.0,
                                theme::TEXT_MUTED.to_array(),
                            ));
                        }
                        y += params.line_height;
                        // No item_idx increment - this is not a selectable item
                    } else {
                        for branch in branches {
                            let visible = y >= params.content_top && y < params.bounds.bottom();
                            if visible {
                                let branch_color = self.layout_item_highlight(
                                    output, &params.inner, y, params.line_height, item_idx,
                                    theme::BRANCH_REMOTE.to_array(), theme::TEXT_BRIGHT.to_array(),
                                );
                                // Remote branch icon
                                output.text_vertices.extend(params.text_renderer.layout_text(
                                    "\u{25CB}", // ○
                                    params.inner.x + params.indent * 2.0,
                                    y + 2.0,
                                    theme::TEXT_MUTED.to_array(),
                                ));
                                let icon_width = params.text_renderer.measure_text("\u{25CB}") + 4.0;
                                let display_name = truncate_to_width(branch, params.text_renderer, params.inner.width - params.indent * 2.0 - icon_width);
                                output.text_vertices.extend(params.text_renderer.layout_text(
                                    &display_name,
                                    params.inner.x + params.indent * 2.0 + icon_width,
                                    y + 2.0,
                                    branch_color,
                                ));
                            }
                            y += params.line_height;
                            item_idx += 1;
                        }
                    }
                }
            }
        }

        (y, item_idx)
    }

    /// Layout the TAGS section. Returns (new_y, new_item_idx).
    fn layout_tags_section(
        &self,
        params: &LayoutParams,
        bold_renderer: &TextRenderer,
        output: &mut WidgetOutput,
        filtered_tags: &[String],
        y: f32,
        item_idx: usize,
    ) -> (f32, usize) {
        let mut y = y;
        let mut item_idx = item_idx;

        y = self.layout_section_header(
            params.text_renderer, bold_renderer, output, &params.inner, y,
            "TAGS", filtered_tags.len(), self.tags_collapsed,
            params.section_header_height, &params.bounds, params.content_top,
        );
        item_idx += 1; // SectionHeader("TAGS")

        if !self.tags_collapsed {
            for tag in filtered_tags {
                let visible = y >= params.content_top && y < params.bounds.bottom();
                if visible {
                    let tag_color = self.layout_item_highlight(
                        output, &params.inner, y, params.line_height, item_idx,
                        theme::BRANCH_RELEASE.to_array(), theme::TEXT_BRIGHT.to_array(),
                    );
                    // Tag icon prefix
                    output.text_vertices.extend(params.text_renderer.layout_text(
                        "\u{2691}", // ⚑
                        params.inner.x + params.indent,
                        y + 2.0,
                        theme::BRANCH_RELEASE.to_array(),
                    ));
                    let icon_width = params.text_renderer.measure_text("\u{2691}") + 4.0;
                    let display_name = truncate_to_width(tag, params.text_renderer, params.inner.width - params.indent - icon_width);
                    output.text_vertices.extend(params.text_renderer.layout_text(
                        &display_name,
                        params.inner.x + params.indent + icon_width,
                        y + 2.0,
                        tag_color,
                    ));
                }
                y += params.line_height;
                item_idx += 1;
            }
        }

        (y, item_idx)
    }

    /// Layout the STASHES section. Returns (new_y, new_item_idx).
    fn layout_stashes_section(
        &self,
        params: &LayoutParams,
        bold_renderer: &TextRenderer,
        output: &mut WidgetOutput,
        filtered_stashes: &[(usize, String, i64)],
        y: f32,
        item_idx: usize,
    ) -> (f32, usize) {
        let mut y = y;
        let mut item_idx = item_idx;

        y = self.layout_section_header(
            params.text_renderer, bold_renderer, output, &params.inner, y,
            "STASHES", filtered_stashes.len(), self.stashes_collapsed,
            params.section_header_height, &params.bounds, params.content_top,
        );
        item_idx += 1; // SectionHeader("STASHES")

        if !self.stashes_collapsed {
            for (stash_index, stash_msg, stash_time) in filtered_stashes {
                let visible = y >= params.content_top && y < params.bounds.bottom();
                if visible {
                    let name_color = self.layout_item_highlight(
                        output, &params.inner, y, params.line_height, item_idx,
                        theme::TEXT.to_array(), theme::TEXT_BRIGHT.to_array(),
                    );

                    // Stash icon: ◈
                    let icon = "\u{25C8}"; // ◈
                    output.text_vertices.extend(params.text_renderer.layout_text(
                        icon,
                        params.inner.x + params.indent,
                        y + 2.0,
                        theme::TEXT_MUTED.to_array(),
                    ));
                    let icon_width = params.text_renderer.measure_text(icon) + 4.0;

                    // Show relative time on the right side
                    let time_str = if *stash_time > 0 {
                        format_relative_time(*stash_time)
                    } else {
                        String::new()
                    };
                    let time_width = if !time_str.is_empty() {
                        params.text_renderer.measure_text(&time_str) + 8.0
                    } else {
                        0.0
                    };

                    // Display the stash message (e.g. "stash@{0}: WIP on main")
                    let label = format!("@{{{}}}: {}", stash_index,
                        stash_msg.split(": ").skip(1).collect::<Vec<_>>().join(": ")
                            .chars().take(60).collect::<String>()
                    );
                    let display_name = truncate_to_width(&label, params.text_renderer, params.inner.width - params.indent - icon_width - time_width);
                    output.text_vertices.extend(params.text_renderer.layout_text(
                        &display_name,
                        params.inner.x + params.indent + icon_width,
                        y + 2.0,
                        name_color,
                    ));

                    // Right-aligned time
                    if !time_str.is_empty() {
                        let time_x = params.inner.right() - params.text_renderer.measure_text(&time_str);
                        output.text_vertices.extend(params.text_renderer.layout_text(
                            &time_str,
                            time_x,
                            y + 2.0,
                            theme::TEXT_MUTED.to_array(),
                        ));
                    }
                }
                y += params.line_height;
                item_idx += 1;
            }
        }

        (y, item_idx)
    }

    /// Compute the total content height for scroll clamping.
    fn compute_content_height(
        &self,
        data: &FilteredData,
        filter_bar_h: f32,
        line_height: f32,
        section_gap: f32,
        show_local: bool,
        show_remote: bool,
        show_tags: bool,
        show_stashes: bool,
    ) -> f32 {
        let section_header_total = self.section_header_total_height();
        let mut total_h: f32 = filter_bar_h;

        if show_local {
            total_h += section_header_total;
            if !self.local_collapsed {
                total_h += data.local.len() as f32 * line_height;
            }
            total_h += section_gap;
        }
        if show_remote {
            total_h += section_header_total;
            if !self.remote_collapsed {
                for (remote_name, branches) in &data.remotes {
                    total_h += line_height; // remote name sub-header
                    if !self.collapsed_remotes.contains(remote_name) {
                        if branches.is_empty() {
                            total_h += line_height; // hint line for empty remote
                        } else {
                            total_h += branches.len() as f32 * line_height;
                        }
                    }
                }
            }
            total_h += section_gap;
        }
        if show_tags {
            total_h += section_header_total;
            if !self.tags_collapsed {
                total_h += data.tags.len() as f32 * line_height;
            }
        }
        if show_stashes {
            total_h += section_gap;
            total_h += section_header_total;
            if !self.stashes_collapsed {
                total_h += data.stashes.len() as f32 * line_height;
            }
        }

        total_h
    }

    /// Layout the sidebar and produce rendering output.
    /// `bold_renderer` is used for section headers (LOCAL, REMOTE, etc.).
    pub fn layout(&mut self, text_renderer: &TextRenderer, bold_renderer: &TextRenderer, bounds: Rect) -> WidgetOutput {
        let mut output = WidgetOutput::new();
        self.last_bounds = Some(bounds);

        self.build_visible_items();

        // Panel background
        output.spline_vertices.extend(create_rect_vertices(
            &bounds,
            theme::PANEL_SIDEBAR.to_array(),
        ));

        let padding = 8.0;
        let inner = bounds.inset(padding);
        let line_height = self.line_height;
        let section_gap = 8.0;
        let filter_bar_h = self.filter_bar_height();

        // The top Y below which scrollable content is visible (below the filter bar)
        let content_top = bounds.y + padding + filter_bar_h;

        // Build filtered data once (shared across section methods and content height)
        let data = self.build_filtered_data();
        let filtering = !self.filter_query.is_empty();

        let show_local = !filtering || !data.local.is_empty();
        let show_remote = !filtering || !data.remotes.is_empty();
        let show_tags = !filtering || !data.tags.is_empty();
        let show_stashes = !self.stashes.is_empty() && (!filtering || !data.stashes.is_empty());

        let params = LayoutParams {
            text_renderer,
            inner,
            bounds,
            line_height,
            section_header_height: self.section_header_height,
            indent: 12.0,
            content_top,
        };

        let mut y = inner.y + filter_bar_h - self.scroll_offset;
        let mut item_idx: usize = 0;

        if show_local {
            (y, item_idx) = self.layout_local_section(&params, bold_renderer, &mut output, &data.local, y, item_idx);
            y += section_gap;
        }

        if show_remote {
            (y, item_idx) = self.layout_remote_section(&params, bold_renderer, &mut output, &data.remotes, y, item_idx);
            y += section_gap;
        }

        if show_tags {
            (y, item_idx) = self.layout_tags_section(&params, bold_renderer, &mut output, &data.tags, y, item_idx);
        }

        if show_stashes {
            y += section_gap;
            (y, item_idx) = self.layout_stashes_section(&params, bold_renderer, &mut output, &data.stashes, y, item_idx);
        }

        // Silence unused warnings for final y/item_idx
        let _ = (y, item_idx);

        // Compute total content height for scroll clamping
        self.content_height = self.compute_content_height(
            &data, filter_bar_h, line_height, section_gap,
            show_local, show_remote, show_tags,
            show_stashes,
        );

        // Update and render scrollbar
        let scrollbar_width = theme::SCROLLBAR_WIDTH;
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

        // Render filter bar LAST as overlay so it covers any items that scroll behind it.
        // First draw a solid background to occlude scrolled content beneath.
        let filter_cover = Rect::new(
            bounds.x,
            bounds.y,
            bounds.width,
            padding + filter_bar_h,
        );
        output.spline_vertices.extend(create_rect_vertices(
            &filter_cover,
            theme::PANEL_SIDEBAR.to_array(),
        ));
        self.layout_filter_bar(text_renderer, &mut output, &bounds);

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
        content_top: f32,
    ) -> f32 {
        let top_pad = 2.0;
        let bottom_pad = 2.0;
        let inset = 6.0;
        let total_height = top_pad + header_height + bottom_pad;
        let visible = y >= content_top && y < bounds.bottom();
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

            // Collapse indicator - small spline triangle chevron
            let chevron_size = text_h * 0.55;
            let chevron_x = inner.x + inset + 4.0;
            let chevron_y = text_y + (text_h - chevron_size) * 0.5;
            output.spline_vertices.extend(create_chevron_vertices(
                chevron_x, chevron_y, chevron_size, collapsed, label_color,
            ));

            // Section title in regular weight, muted color, smaller scale
            let indicator_width = chevron_size + 4.0;
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
