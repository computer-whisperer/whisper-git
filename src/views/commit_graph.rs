use std::collections::{HashMap, HashSet};

use git2::Oid;

use crate::git::{BranchTip, CommitInfo, TagInfo, WorkingDirStatus, WorktreeInfo};
use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::avatar::{self, AvatarCache, AvatarRenderer};
use crate::ui::widget::{
    create_rect_vertices, create_rounded_rect_vertices,
    create_rounded_rect_outline_vertices, theme,
};
use crate::ui::widgets::context_menu::MenuItem;
use crate::ui::widgets::scrollbar::{Scrollbar, ScrollAction};
use crate::ui::widgets::search_bar::{SearchBar, SearchAction};
use crate::ui::{Color, Rect, Spline, SplinePoint, SplineVertex, TextRenderer, TextVertex};
use crate::ui::text_util::truncate_to_width;

use crate::ui::widget::theme::LANE_COLORS;
use crate::views::staging_well::compute_display_names;

/// Actions emitted by the commit graph view
pub enum GraphAction {
    /// Request to load more commits (infinite scroll)
    LoadMore,
    /// Switch the staging panel to a different worktree
    SwitchWorktree(String),
}

/// Click target for a worktree/working pill in the commit graph
struct PillClickTarget {
    worktree_name: String,
    bounds: [f32; 4], // x, y, width, height
}

/// Layout information for a single commit
#[derive(Clone, Debug)]
pub struct CommitLayout {
    pub lane: usize,
    pub color: Color,
}

/// Graph layout algorithm that assigns lanes to commits
pub struct GraphLayout {
    /// Map from commit ID to layout info
    layouts: HashMap<Oid, CommitLayout>,
    /// Active lanes (which commit ID occupies each lane, if any)
    active_lanes: Vec<Option<Oid>>,
    /// Highest lane index that was active at any point (for graph width)
    max_active_lane: usize,
}

impl GraphLayout {
    pub fn new() -> Self {
        Self {
            layouts: HashMap::new(),
            active_lanes: Vec::new(),
            max_active_lane: 0,
        }
    }

    /// Update the peak active lane index
    fn update_peak(&mut self) {
        // Find the highest occupied lane index
        for (i, occupant) in self.active_lanes.iter().enumerate().rev() {
            if occupant.is_some() {
                if i > self.max_active_lane {
                    self.max_active_lane = i;
                }
                return;
            }
        }
    }

    /// Find the lowest-numbered free lane, or allocate a new one
    fn lowest_free_lane(&mut self) -> usize {
        for (lane, occupant) in self.active_lanes.iter().enumerate() {
            if occupant.is_none() {
                return lane;
            }
        }
        let lane = self.active_lanes.len();
        self.active_lanes.push(None);
        lane
    }

    /// Build layout for a list of commits (should be in topological order)
    pub fn build(&mut self, commits: &[CommitInfo]) {
        self.layouts.clear();
        self.active_lanes.clear();
        self.max_active_lane = 0;

        // Map from commit ID to its index for quick parent lookup
        let commit_indices: HashMap<Oid, usize> = commits
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id, i))
            .collect();

        for commit in commits.iter() {
            // Step 1: Find lane for this commit (may already be reserved)
            let lane = self.find_or_assign_lane(commit);
            let color = if commit.is_orphaned {
                theme::ORPHAN
            } else {
                LANE_COLORS[lane % LANE_COLORS.len()]
            };

            self.layouts.insert(commit.id, CommitLayout { lane, color });

            // Step 2: Free any OTHER lanes that were also tracking this commit
            // (happens when multiple children pointed to the same parent)
            for i in 0..self.active_lanes.len() {
                if i != lane && self.active_lanes[i] == Some(commit.id) {
                    self.active_lanes[i] = None;
                }
            }

            // Step 3: Update active lanes based on this commit's parents
            self.update_lanes_for_parents(commit, lane, &commit_indices);

            // Step 4: Track peak active lanes for graph width
            self.update_peak();
        }
    }

    fn find_or_assign_lane(
        &mut self,
        commit: &CommitInfo,
    ) -> usize {
        // Check if any active lane is already waiting for this commit
        // (reserved by a child's update_lanes_for_parents)
        for (lane, occupant) in self.active_lanes.iter().enumerate() {
            if *occupant == Some(commit.id) {
                return lane;
            }
        }

        // No lane reserved -- this is a branch head (tip) or orphan.
        // Assign to the lowest free lane for compactness.
        let lane = self.lowest_free_lane();

        // Ensure the lane exists in the vector
        while self.active_lanes.len() <= lane {
            self.active_lanes.push(None);
        }

        lane
    }

    fn update_lanes_for_parents(
        &mut self,
        commit: &CommitInfo,
        commit_lane: usize,
        commit_indices: &HashMap<Oid, usize>,
    ) {
        // Ensure we have enough lanes
        while self.active_lanes.len() <= commit_lane {
            self.active_lanes.push(None);
        }

        if commit.parent_ids.is_empty() {
            // Root commit - free the lane
            self.active_lanes[commit_lane] = None;
            return;
        }

        // First parent continues in the same lane (straight line down)
        let first_parent = commit.parent_ids[0];
        if commit_indices.contains_key(&first_parent) {
            self.active_lanes[commit_lane] = Some(first_parent);
        } else {
            // Parent not in our visible set - free the lane
            self.active_lanes[commit_lane] = None;
        }

        // Secondary parents (merge sources) need their own lanes
        for &parent_id in commit.parent_ids.iter().skip(1) {
            if !commit_indices.contains_key(&parent_id) {
                continue;
            }

            // Check if another lane is already tracking this parent
            let already_tracked = self.active_lanes.contains(&Some(parent_id));
            if already_tracked {
                continue;
            }

            // Assign this secondary parent to the lowest free lane
            let lane = self.lowest_free_lane();
            while self.active_lanes.len() <= lane {
                self.active_lanes.push(None);
            }
            self.active_lanes[lane] = Some(parent_id);
        }
    }

    /// Get layout for a commit
    pub fn get(&self, id: &Oid) -> Option<&CommitLayout> {
        self.layouts.get(id)
    }

}

impl Default for GraphLayout {
    fn default() -> Self {
        Self::new()
    }
}

/// View for displaying a commit graph with branch visualization
pub struct CommitGraphView {
    layout: GraphLayout,
    line_width: f32,
    lane_width: f32,
    row_height: f32,
    node_radius: f32,
    segments_per_curve: usize,
    /// Currently selected commit
    pub selected_commit: Option<Oid>,
    /// Currently hovered commit
    pub hovered_commit: Option<Oid>,
    /// Working directory status
    pub working_dir_status: Option<WorkingDirStatus>,
    /// HEAD commit OID
    pub head_oid: Option<Oid>,
    /// Branch tips for labels
    pub branch_tips: Vec<BranchTip>,
    /// Tags
    pub tags: Vec<TagInfo>,
    /// Worktrees (for WT: pills on commits)
    pub worktrees: Vec<WorktreeInfo>,
    /// Row scale factor (1.0 = normal, 1.5 = large)
    pub row_scale: f32,
    /// Scroll offset
    pub scroll_offset: f32,
    /// Scrollbar widget
    pub scrollbar: Scrollbar,
    /// Search bar widget
    pub search_bar: SearchBar,
    /// Ordered list of commit indices that match the current search query
    search_matches: Vec<usize>,
    /// Pending action to be consumed by the app
    pending_action: Option<GraphAction>,
    /// Guard to prevent rapid-fire LoadMore requests
    loading_more: bool,
    /// Pre-computed Y offsets for each row (time-based variable spacing)
    row_y_offsets: Vec<f32>,
    /// Click targets for worktree/working pills (rebuilt each layout_text call)
    pill_click_targets: Vec<PillClickTarget>,
    /// Whether the mouse is currently hovering over a clickable pill
    pub hovered_pill: bool,
    /// Whether to abbreviate worktree names in pills (strip common prefix)
    pub abbreviate_worktree_names: bool,
    /// Time spacing strength multiplier (0.3 = low, 1.0 = normal, 2.0 = high)
    pub time_spacing_strength: f32,
    /// Cached adaptive graph width based on visible rows (updated each frame)
    adaptive_graph_width: f32,
}

impl Default for CommitGraphView {
    fn default() -> Self {
        Self {
            layout: GraphLayout::new(),
            line_width: 2.5,
            lane_width: 22.0,
            row_height: 24.0,
            node_radius: 6.0,
            segments_per_curve: 20,
            row_scale: 1.0,
            selected_commit: None,
            hovered_commit: None,
            working_dir_status: None,
            head_oid: None,
            branch_tips: Vec::new(),
            tags: Vec::new(),
            worktrees: Vec::new(),
            scroll_offset: 0.0,
            scrollbar: Scrollbar::new(),
            search_bar: SearchBar::new(),
            search_matches: Vec::new(),
            pending_action: None,
            loading_more: false,
            row_y_offsets: Vec::new(),
            pill_click_targets: Vec::new(),
            hovered_pill: false,
            adaptive_graph_width: 0.0,
            abbreviate_worktree_names: true,
            time_spacing_strength: 1.0,
        }
    }
}

impl CommitGraphView {
    /// Update layout constants to match the current text renderer metrics.
    /// Call this when the display scale changes or at startup.
    pub fn sync_metrics(&mut self, text_renderer: &TextRenderer) {
        let lh = text_renderer.line_height();
        let s = self.row_scale;
        self.row_height = (lh * 1.55 * s).max(18.0 * s);   // ~24.8px at s=1.0, lh=16 (tighter rows)
        self.lane_width = (lh * 1.0 * s).max(12.0 * s);    // ~16px (compact lanes)
        self.node_radius = (lh * 0.38 * s).max(5.0 * s);   // ~6px (larger donut nodes)
        self.line_width = (lh * 0.18 * s.sqrt()).max(2.5);  // ~2.9px (thicker lines)
    }
}

impl CommitGraphView {
    pub fn new() -> Self {
        Self::default()
    }

    /// Header offset above graph content, scales with row_height.
    fn header_offset(&self) -> f32 {
        self.row_height * 0.35
    }

    /// Width reserved for the scrollbar track
    fn scrollbar_width(&self) -> f32 {
        (self.lane_width * 0.6).max(8.0)
    }

    /// Height of the search bar overlay
    fn search_bar_height(&self) -> f32 {
        self.row_height
    }

    /// Horizontal padding for the search bar from graph edges
    fn search_bar_pad(&self) -> f32 {
        self.lane_width * 2.0
    }

    /// Left padding before the first lane
    fn lane_left_pad(&self) -> f32 {
        self.lane_width * 0.5
    }

    /// Take the pending action, if any
    pub fn take_action(&mut self) -> Option<GraphAction> {
        self.pending_action.take()
    }

    /// Signal that loading has completed (resets the loading guard)
    pub fn finish_loading(&mut self) {
        self.loading_more = false;
    }

    /// Total content height based on row offsets (or fallback to uniform spacing)
    fn total_content_height(&self, commit_count: usize) -> f32 {
        if let Some(&last_offset) = self.row_y_offsets.last() {
            last_offset + self.row_height
        } else {
            commit_count as f32 * self.row_height
        }
    }

    /// Check if we're near the bottom and should request more commits
    fn check_load_more(&mut self, commits: &[CommitInfo], bounds: Rect) {
        if self.loading_more {
            return;
        }
        let total_h = self.total_content_height(commits.len());
        let threshold = total_h - bounds.height - self.row_height * 5.0;
        if self.scroll_offset >= threshold.max(0.0) {
            self.loading_more = true;
            self.pending_action = Some(GraphAction::LoadMore);
        }
    }

    /// Update layout for the given commits
    pub fn update_layout(&mut self, commits: &[CommitInfo]) {
        self.layout.build(commits);
        self.compute_row_offsets(commits);
    }

    /// Compute accumulated Y offsets for each row based on time deltas between
    /// consecutive commits. Adjacent commits get minimal spacing; distant commits
    /// get proportionally more gap (log-scaled, capped at 2x row_height).
    ///
    /// The max_gap multiplier is kept at 2.0 (rather than 3.0) so the spacing
    /// difference between adjacent and distant commits is less dramatic, and the
    /// base_seconds reference is 7200 (2 hours) to further smooth the log curve
    /// and reduce sensitivity to small time differences.
    pub(crate) fn compute_row_offsets(&mut self, commits: &[CommitInfo]) {
        self.row_y_offsets.clear();
        if commits.is_empty() {
            return;
        }

        let min_gap = self.row_height;
        let max_gap = self.row_height * 2.0;
        let base_seconds: f64 = 7200.0; // 2 hour reference (smooths log curve)
        let max_delta: f64 = 30.0 * 24.0 * 3600.0; // 30 days caps scaling
        let log_max = (1.0 + max_delta / base_seconds).ln();

        let mut accumulated = 0.0f32;
        self.row_y_offsets.push(0.0); // first row starts at 0

        for i in 1..commits.len() {
            // Commits are in reverse chronological order (newer first)
            let delta_seconds = (commits[i - 1].time - commits[i].time).unsigned_abs() as f64;
            let clamped_delta = delta_seconds.min(max_delta);
            let ratio = (1.0 + clamped_delta / base_seconds).ln() / log_max;
            let gap = min_gap + (max_gap - min_gap) * ratio as f32 * self.time_spacing_strength;
            accumulated += gap;
            self.row_y_offsets.push(accumulated);
        }
    }

    /// Calculate the width needed for the graph portion based on global max lane
    fn graph_width_for_lanes(&self, max_lane: usize) -> f32 {
        let lanes = (max_lane + 1).max(1);
        let computed = lanes as f32 * self.lane_width + self.lane_width * 0.5;
        // Smaller minimum for compact layout
        computed.max(self.lane_width * 1.5)
    }

    /// Update the adaptive graph width based on the max lane visible on screen.
    /// Scans only commits that are within the visible viewport bounds.
    fn update_adaptive_graph_width(&mut self, commits: &[CommitInfo], bounds: &Rect) {
        let header_offset = self.header_offset();
        let buffer = self.row_height * 5.0;
        let mut max_visible_lane: usize = 0;

        for (row, commit) in commits.iter().enumerate() {
            let y = self.row_y(row, bounds, header_offset);
            // Only consider rows visible on screen (with a small buffer)
            if y < bounds.y - buffer || y > bounds.bottom() + buffer {
                continue;
            }
            if let Some(layout) = self.layout.get(&commit.id) {
                if layout.lane > max_visible_lane {
                    max_visible_lane = layout.lane;
                }
            }
        }

        self.adaptive_graph_width = self.graph_width_for_lanes(max_visible_lane);
    }

    /// Get x position for a lane
    fn lane_x(&self, lane: usize, bounds: &Rect) -> f32 {
        bounds.x + self.lane_left_pad() + lane as f32 * self.lane_width + self.lane_width / 2.0
    }

    /// Get y position for a row (adjusted for scroll)
    fn row_y(&self, row: usize, bounds: &Rect, header_offset: f32) -> f32 {
        let y_offset = self.row_y_offsets.get(row).copied()
            .unwrap_or(row as f32 * self.row_height);
        bounds.y + header_offset + y_offset
            + self.row_height / 2.0
            - self.scroll_offset
    }

    /// Find the row index at a given Y position using binary search on row_y_offsets.
    /// Returns None if the position doesn't correspond to any row.
    fn row_at_y(&self, y: f32, bounds: &Rect, header_offset: f32, commit_count: usize) -> Option<usize> {
        if commit_count == 0 {
            return None;
        }
        // Convert screen Y to content-space Y (offset from the top of the content area)
        let content_y = y - bounds.y - header_offset + self.scroll_offset;

        if self.row_y_offsets.len() == commit_count {
            // Binary search: find the row whose center (offset + row_height/2) is closest to content_y
            // content_y should match offset + row_height/2, so look for offset closest to content_y - row_height/2
            let target = content_y - self.row_height / 2.0;
            let idx = self.row_y_offsets.partition_point(|&off| off < target);
            // Check idx-1 and idx to find closest
            let mut best_row = None;
            let mut best_dist = f32::MAX;
            for candidate in [idx.saturating_sub(1), idx.min(commit_count - 1)] {
                let row_center_y = self.row_y_offsets[candidate] + self.row_height / 2.0;
                let dist = (content_y - row_center_y).abs();
                if dist < self.row_height / 2.0 && dist < best_dist {
                    best_dist = dist;
                    best_row = Some(candidate);
                }
            }
            best_row
        } else {
            // Fallback: uniform spacing
            let row_center_offset = self.row_height / 2.0;
            let approx_row = ((content_y - row_center_offset) / self.row_height).round() as isize;
            if approx_row >= 0 && (approx_row as usize) < commit_count {
                Some(approx_row as usize)
            } else {
                None
            }
        }
    }

    /// Handle input events
    pub fn handle_event(
        &mut self,
        event: &InputEvent,
        commits: &[CommitInfo],
        bounds: Rect,
    ) -> EventResponse {
        let header_offset = self.header_offset();
        let scrollbar_width = self.scrollbar_width();

        // Calculate scrollbar bounds (right edge of graph area)
        let (content_bounds, scrollbar_bounds) = bounds.take_right(scrollbar_width);

        // Search bar bounds (overlay at top of graph area)
        let search_bar_height = self.search_bar_height();
        let search_bar_pad = self.search_bar_pad();
        let search_bounds = Rect::new(
            bounds.x + search_bar_pad,
            bounds.y + 4.0,
            bounds.width - search_bar_pad * 2.0 - scrollbar_width,
            search_bar_height,
        );

        // Handle search bar activation via Ctrl+F or /
        if let InputEvent::KeyDown { key, modifiers, .. } = event
            && ((*key == Key::F && modifiers.only_ctrl()) || (*key == Key::Slash && !modifiers.any() && !self.search_bar.is_active())) {
                self.search_bar.activate();
                return EventResponse::Consumed;
            }

        // Route events to search bar first when active
        if self.search_bar.is_active()
            && self.search_bar.handle_event(event, search_bounds).is_consumed() {
                // Process search actions
                if let Some(action) = self.search_bar.take_action() {
                    match action {
                        SearchAction::QueryChanged(query) => {
                            self.update_search_matches(&query, commits);
                            // Auto-navigate to first match on query change
                            self.navigate_to_current_match(commits, bounds);
                        }
                        SearchAction::Navigate => {
                            self.navigate_to_current_match(commits, bounds);
                        }
                        SearchAction::Closed => {
                            self.search_matches.clear();
                        }
                    }
                }
                return EventResponse::Consumed;
            }

        // Route events to scrollbar
        if self.scrollbar.handle_event(event, scrollbar_bounds).is_consumed() {
            if let Some(ScrollAction::ScrollTo(ratio)) = self.scrollbar.take_action() {
                let max_scroll = (self.total_content_height(commits.len()) - bounds.height + self.row_height * 2.0).max(0.0);
                self.scroll_offset = (ratio * max_scroll).clamp(0.0, max_scroll);
            }
            self.check_load_more(commits, bounds);
            return EventResponse::Consumed;
        }

        match event {
            InputEvent::KeyDown { key, .. } => match key {
                Key::J | Key::Down => {
                    // Move selection down
                    self.move_selection(1, commits);
                    self.scroll_to_selection(commits, bounds);
                    self.check_load_more(commits, bounds);
                    EventResponse::Consumed
                }
                Key::K | Key::Up => {
                    // Move selection up
                    self.move_selection(-1, commits);
                    self.scroll_to_selection(commits, bounds);
                    EventResponse::Consumed
                }
                Key::PageDown => {
                    let visible_rows = (bounds.height / self.row_height).max(1.0) as i32;
                    self.move_selection(visible_rows, commits);
                    self.scroll_to_selection(commits, bounds);
                    self.check_load_more(commits, bounds);
                    EventResponse::Consumed
                }
                Key::PageUp => {
                    let visible_rows = (bounds.height / self.row_height).max(1.0) as i32;
                    self.move_selection(-visible_rows, commits);
                    self.scroll_to_selection(commits, bounds);
                    EventResponse::Consumed
                }
                Key::G => {
                    // Go to HEAD
                    if let Some(head) = self.head_oid {
                        self.selected_commit = Some(head);
                        self.scroll_to_selection(commits, bounds);
                    }
                    EventResponse::Consumed
                }
                Key::Home => {
                    // Go to first commit
                    if let Some(commit) = commits.first() {
                        self.selected_commit = Some(commit.id);
                        self.scroll_offset = 0.0;
                    }
                    EventResponse::Consumed
                }
                Key::End => {
                    // Go to last commit
                    if let Some(commit) = commits.last() {
                        self.selected_commit = Some(commit.id);
                        self.scroll_to_selection(commits, bounds);
                    }
                    self.check_load_more(commits, bounds);
                    EventResponse::Consumed
                }
                _ => EventResponse::Ignored,
            },
            InputEvent::MouseDown {
                button: MouseButton::Left,
                x,
                y,
                ..
            } => {
                if content_bounds.contains(*x, *y) {
                    // Check for click on a worktree/working pill first (more specific target)
                    for target in &self.pill_click_targets {
                        let [px, py, pw, ph] = target.bounds;
                        if *x >= px && *x <= px + pw && *y >= py && *y <= py + ph {
                            self.pending_action = Some(GraphAction::SwitchWorktree(target.worktree_name.clone()));
                            return EventResponse::Consumed;
                        }
                    }
                    // Check for click on a commit using binary search
                    if let Some(row) = self.row_at_y(*y, &bounds, header_offset, commits.len()) {
                        if let Some(commit) = commits.get(row) {
                            self.selected_commit = Some(commit.id);
                            return EventResponse::Consumed;
                        }
                    }
                }
                EventResponse::Ignored
            }
            InputEvent::MouseMove { x, y, .. } => {
                // Check if hovering over a clickable pill
                self.hovered_pill = false;
                for target in &self.pill_click_targets {
                    let [px, py, pw, ph] = target.bounds;
                    if *x >= px && *x <= px + pw && *y >= py && *y <= py + ph {
                        self.hovered_pill = true;
                        break;
                    }
                }
                // Update hover state using binary search
                self.hovered_commit = None;
                if content_bounds.contains(*x, *y) {
                    if let Some(row) = self.row_at_y(*y, &bounds, header_offset, commits.len()) {
                        if let Some(commit) = commits.get(row) {
                            self.hovered_commit = Some(commit.id);
                        }
                    }
                }
                // Also update scrollbar hover
                self.scrollbar.handle_event(event, scrollbar_bounds);
                EventResponse::Ignored // Don't consume move events
            }
            InputEvent::Scroll { delta_y, x, y, .. } => {
                if bounds.contains(*x, *y) {
                    let max_scroll = (self.total_content_height(commits.len()) - bounds.height + self.row_height * 2.0).max(0.0);
                    self.scroll_offset = (self.scroll_offset - delta_y * 2.0).max(0.0).min(max_scroll);
                    self.check_load_more(commits, bounds);
                    EventResponse::Consumed
                } else {
                    EventResponse::Ignored
                }
            }
            _ => EventResponse::Ignored,
        }
    }

    /// Get context menu items for the commit at (x, y), if any.
    /// Also selects the commit under the cursor. Returns (items, commit_oid).
    pub fn context_menu_items_at(
        &mut self,
        x: f32,
        y: f32,
        commits: &[CommitInfo],
        bounds: Rect,
    ) -> Option<(Vec<MenuItem>, Oid)> {
        let header_offset = self.header_offset();
        let scrollbar_width = self.scrollbar_width();
        let (content_bounds, _) = bounds.take_right(scrollbar_width);

        if !content_bounds.contains(x, y) {
            return None;
        }

        if let Some(row) = self.row_at_y(y, &bounds, header_offset, commits.len()) {
            if let Some(commit) = commits.get(row) {
                // No context menu for synthetic "uncommitted changes" rows
                if commit.is_synthetic {
                    return None;
                }

                self.selected_commit = Some(commit.id);

                let mut items = vec![
                    MenuItem::new("Copy SHA", "copy_sha"),
                    MenuItem::new("View Details", "view_details"),
                ];

                // Check if this commit has any branch labels
                let has_branch = self.branch_tips.iter().any(|t| t.oid == commit.id && !t.is_remote);
                if has_branch {
                    items.push(MenuItem::new("Checkout", "checkout"));
                }

                // Merge/rebase for non-HEAD local branches at this commit
                let head_branch = self.branch_tips.iter()
                    .find(|t| t.is_head)
                    .map(|t| t.name.as_str())
                    .unwrap_or("HEAD");
                let mergeable_branches: Vec<&str> = self.branch_tips.iter()
                    .filter(|t| t.oid == commit.id && !t.is_remote && !t.is_head)
                    .map(|t| t.name.as_str())
                    .collect();
                for name in &mergeable_branches {
                    items.push(MenuItem::new(
                        &format!("Merge '{}' into '{}'", name, head_branch),
                        &format!("merge:{}", name),
                    ));
                }
                for name in &mergeable_branches {
                    items.push(MenuItem::new(
                        &format!("Rebase '{}' onto '{}'", head_branch, name),
                        &format!("rebase:{}", name),
                    ));
                }

                // Remote branches at this commit
                let remote_branches: Vec<String> = self.branch_tips.iter()
                    .filter(|t| t.oid == commit.id && t.is_remote)
                    .map(|t| t.name.clone())
                    .collect();
                for name in &remote_branches {
                    items.push(MenuItem::new(
                        &format!("Merge '{}' into '{}'", name, head_branch),
                        &format!("merge:{}", name),
                    ));
                    items.push(MenuItem::new(
                        &format!("Rebase '{}' onto '{}'", head_branch, name),
                        &format!("rebase:{}", name),
                    ));
                }

                items.push(MenuItem::separator());
                items.push(MenuItem::new(
                    &format!("Cherry-pick into '{}'", head_branch), "cherry_pick"
                ));
                items.push(MenuItem::new(
                    &format!("Revert on '{}'", head_branch), "revert_commit"
                ));
                items.push(MenuItem::new("Create Branch Here", "create_branch"));
                items.push(MenuItem::new("Create Worktree Here", "create_worktree"));
                items.push(MenuItem::new("Create Tag Here", "create_tag"));
                items.push(MenuItem::separator());
                items.push(MenuItem::new(
                    &format!("Reset '{}' Soft to Here", head_branch), "reset_soft"
                ));
                items.push(MenuItem::new(
                    &format!("Reset '{}' Mixed to Here", head_branch), "reset_mixed"
                ));
                items.push(MenuItem::new(
                    &format!("Reset '{}' Hard to Here", head_branch), "reset_hard"
                ));

                return Some((items, commit.id));
            }
        }

        None
    }

    /// Update search matches based on query
    fn update_search_matches(&mut self, query: &str, commits: &[CommitInfo]) {
        self.search_matches.clear();
        if query.is_empty() {
            self.search_bar.set_match_count(0);
            return;
        }

        let query_lower = query.to_lowercase();
        for (idx, commit) in commits.iter().enumerate() {
            if commit.summary.to_lowercase().contains(&query_lower)
                || commit.author.to_lowercase().contains(&query_lower)
                || commit.short_id.to_lowercase().contains(&query_lower)
                || commit.id.to_string().to_lowercase().starts_with(&query_lower)
            {
                self.search_matches.push(idx);
            }
        }
        self.search_bar.set_match_count(self.search_matches.len());
    }

    /// Navigate to the current search match: select and scroll to it
    fn navigate_to_current_match(&mut self, commits: &[CommitInfo], bounds: Rect) {
        let match_idx = self.search_bar.current_match();
        if let Some(&commit_idx) = self.search_matches.get(match_idx) {
            if let Some(commit) = commits.get(commit_idx) {
                self.selected_commit = Some(commit.id);
                self.scroll_to_selection(commits, bounds);
            }
        }
    }

    /// Check if a commit index is in the search match set
    fn is_search_match_idx(&self, idx: usize) -> bool {
        if !self.search_bar.is_active() || self.search_bar.query().is_empty() {
            return true; // No filter active, everything matches
        }
        self.search_matches.contains(&idx)
    }

    fn move_selection(&mut self, delta: i32, commits: &[CommitInfo]) {
        let current_idx = self
            .selected_commit
            .and_then(|id| commits.iter().position(|c| c.id == id))
            .unwrap_or(0);

        let new_idx = if delta > 0 {
            (current_idx + delta as usize).min(commits.len().saturating_sub(1))
        } else {
            current_idx.saturating_sub((-delta) as usize)
        };

        if let Some(commit) = commits.get(new_idx) {
            self.selected_commit = Some(commit.id);
        }
    }

    pub fn scroll_to_selection(&mut self, commits: &[CommitInfo], bounds: Rect) {
        if let Some(id) = self.selected_commit
            && let Some(idx) = commits.iter().position(|c| c.id == id) {
                let target_y = self.row_y_offsets.get(idx).copied()
                    .unwrap_or(idx as f32 * self.row_height);
                let visible_height = bounds.height - self.row_height * 2.0;

                if target_y < self.scroll_offset {
                    self.scroll_offset = target_y;
                } else if target_y > self.scroll_offset + visible_height {
                    self.scroll_offset = target_y - visible_height + self.row_height;
                }
            }
    }

    /// Generate spline vertices for branch lines and nodes, plus scrollbar and search bar
    pub fn layout_splines(
        &mut self,
        text_renderer: &TextRenderer,
        commits: &[CommitInfo],
        bounds: Rect,
    ) -> Vec<SplineVertex> {
        let mut vertices = Vec::new();
        let header_offset = self.header_offset();
        let scrollbar_width = self.scrollbar_width();

        // Update scrollbar state using total content height (approximate via equivalent row count)
        let total_h = self.total_content_height(commits.len());
        let equivalent_total_rows = (total_h / self.row_height).ceil() as usize;
        let visible_rows = (bounds.height / self.row_height).max(1.0) as usize;
        let scroll_offset_items = (self.scroll_offset / self.row_height).round() as usize;
        self.scrollbar.set_content(equivalent_total_rows, visible_rows, scroll_offset_items);

        // Compute adaptive graph width based on visible rows
        self.update_adaptive_graph_width(commits, &bounds);

        // Background strip for graph column - subtle elevation
        let graph_bg_width = self.adaptive_graph_width + self.lane_width * 1.5;
        let graph_bg = Rect::new(bounds.x, bounds.y, graph_bg_width, bounds.height);
        vertices.extend(create_rect_vertices(
            &graph_bg,
            theme::SURFACE.to_array(),
        ));

        // Subtle separator line between graph and text columns
        let sep_x = bounds.x + graph_bg_width;
        vertices.extend(create_rect_vertices(
            &Rect::new(sep_x, bounds.y, 1.0, bounds.height),
            theme::BORDER.with_alpha(0.3).to_array(),
        ));

        // Shadow gradient to the right of the separator (gives graph column depth)
        let shadow_alphas: &[f32] = &[0.08, 0.05, 0.03, 0.01];
        for (i, &alpha) in shadow_alphas.iter().enumerate() {
            vertices.extend(create_rect_vertices(
                &Rect::new(sep_x + 1.0 + i as f32, bounds.y, 1.0, bounds.height),
                [0.0, 0.0, 0.0, alpha],
            ));
        }

        // Column separator lines between diff stats / time columns
        // (mirrors the column positions computed in layout_text)
        let time_col_width: f32 = 80.0;
        let stats_col_width: f32 = 100.0;
        let right_margin: f32 = 8.0;
        let col_gap: f32 = 16.0;
        let time_col_right = bounds.right() - right_margin - scrollbar_width;
        let time_col_left = time_col_right - time_col_width;
        let stats_col_right = time_col_left - col_gap;
        let stats_col_left = stats_col_right - stats_col_width;
        let separator_color = theme::BORDER.with_alpha(0.20).to_array();

        // Separator between commit message and diff stats
        vertices.extend(create_rect_vertices(
            &Rect::new(stats_col_left - col_gap / 2.0, bounds.y, 1.0, bounds.height),
            separator_color,
        ));
        // Separator between diff stats and time
        vertices.extend(create_rect_vertices(
            &Rect::new(time_col_left - col_gap / 2.0, bounds.y, 1.0, bounds.height),
            separator_color,
        ));

        // Build index for quick parent lookup
        let commit_indices: HashMap<Oid, usize> = commits
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id, i))
            .collect();

        // Pre-pass: render ALL zebra stripes BEFORE connection lines so stripes
        // appear behind everything else (spline vertices render in order).
        for (row, commit) in commits.iter().enumerate() {
            if row % 2 == 0 {
                continue;
            }
            // Skip zebra stripe for selected row (selection highlight replaces it)
            if self.selected_commit == Some(commit.id) {
                continue;
            }
            let y = self.row_y(row, &bounds, header_offset);
            let buffer = self.row_height * 15.0;
            if y < bounds.y - buffer || y > bounds.bottom() + buffer {
                continue;
            }
            // Only render if this commit has a layout entry
            if self.layout.get(&commit.id).is_none() {
                continue;
            }
            let stripe_rect = Rect::new(
                bounds.x,
                y - self.row_height / 2.0,
                bounds.width - scrollbar_width,
                self.row_height,
            );
            vertices.extend(create_rect_vertices(
                &stripe_rect,
                theme::GRAPH_ROW_ALT.to_array(),
            ));
        }

        for (row, commit) in commits.iter().enumerate() {
            let Some(layout) = self.layout.get(&commit.id) else {
                continue;
            };

            let x = self.lane_x(layout.lane, &bounds);
            let y = self.row_y(row, &bounds, header_offset);

            // Skip if outside visible area (15-row buffer for stable connection lines)
            let buffer = self.row_height * 15.0;
            if y < bounds.y - buffer || y > bounds.bottom() + buffer {
                continue;
            }

            // Draw connections to parents
            self.render_graph_connections(
                commit, layout, x, y, buffer,
                &bounds, header_offset, &commit_indices, &mut vertices,
            );

            // Draw commit node with selection/hover highlights and search match overlay
            self.render_commit_node(
                commit, layout, row, x, y,
                &bounds, scrollbar_width, &mut vertices,
            );
        }

        // Render scrollbar
        let (_content_bounds, scrollbar_bounds) = bounds.take_right(scrollbar_width);
        let scrollbar_output = self.scrollbar.layout(scrollbar_bounds);
        vertices.extend(scrollbar_output.spline_vertices);

        // Render search bar overlay
        if self.search_bar.is_active() {
            let search_bar_pad = self.search_bar_pad();
            let search_bounds = Rect::new(
                bounds.x + search_bar_pad,
                bounds.y + 4.0,
                bounds.width - search_bar_pad * 2.0 - scrollbar_width,
                self.search_bar_height(),
            );
            let search_output = self.search_bar.layout(text_renderer, search_bounds);
            vertices.extend(search_output.spline_vertices);
        }

        vertices
    }

    /// Render graph connection lines from a commit to its parents
    fn render_graph_connections(
        &self,
        commit: &CommitInfo,
        layout: &CommitLayout,
        x: f32,
        y: f32,
        buffer: f32,
        bounds: &Rect,
        header_offset: f32,
        commit_indices: &HashMap<Oid, usize>,
        vertices: &mut Vec<SplineVertex>,
    ) {
        for &parent_id in commit.parent_ids.iter() {
            if let Some(&parent_row) = commit_indices.get(&parent_id)
                && let Some(parent_layout) = self.layout.get(&parent_id) {
                    let parent_x = self.lane_x(parent_layout.lane, bounds);
                    let parent_y = self.row_y(parent_row, bounds, header_offset);

                    // Use the child's color for the connection
                    let color = layout.color.to_array();

                    if layout.lane == parent_layout.lane {
                        // Vertical line - same lane
                        let start_y = (y + self.node_radius).max(bounds.y - buffer);
                        let end_y = (parent_y - self.node_radius).min(bounds.bottom() + buffer);
                        let mut spline = Spline::new(
                            SplinePoint::new(x, start_y),
                            color,
                            self.line_width,
                        );
                        spline.line_to(SplinePoint::new(
                            parent_x,
                            end_y,
                        ));
                        vertices.extend(spline.to_vertices(self.segments_per_curve));
                    } else {
                        // Bezier curve - different lanes (merge/fork)
                        let start_y = (y + self.node_radius).max(bounds.y - buffer);
                        let end_y = (parent_y - self.node_radius).min(bounds.bottom() + buffer);
                        let dy = end_y - start_y;

                        let mut spline = Spline::new(
                            SplinePoint::new(x, start_y),
                            color,
                            self.line_width,
                        );

                        // S-curve: drop vertically for 30%, curve across lanes,
                        // then continue vertically to parent
                        let ctrl1 = SplinePoint::new(x, start_y + dy * 0.4);
                        let ctrl2 = SplinePoint::new(parent_x, end_y - dy * 0.4);

                        spline.cubic_to(
                            ctrl1,
                            ctrl2,
                            SplinePoint::new(parent_x, end_y),
                        );
                        vertices.extend(spline.to_vertices(self.segments_per_curve));
                    }
                }
        }
    }

    /// Render the commit node (donut-ring), selection/hover highlights, and search match overlay
    fn render_commit_node(
        &self,
        commit: &CommitInfo,
        layout: &CommitLayout,
        row: usize,
        x: f32,
        y: f32,
        bounds: &Rect,
        scrollbar_width: f32,
        vertices: &mut Vec<SplineVertex>,
    ) {
        let is_merge = commit.parent_ids.len() > 1;
        let is_selected = self.selected_commit == Some(commit.id);
        let is_hovered = self.hovered_commit == Some(commit.id);
        let is_head = self.head_oid == Some(commit.id);
        let is_match = self.is_search_match_idx(row);

        // Dim factor for non-matching commits during search
        let dim_alpha = if is_match { 1.0 } else { 0.2 };

        // Selection/hover highlight (full row)
        if is_selected || is_hovered {
            let highlight_color = if is_selected {
                theme::ACCENT_MUTED
            } else {
                theme::SURFACE_HOVER.with_alpha(0.15)
            };
            let highlight_rect = Rect::new(
                bounds.x,
                y - self.row_height / 2.0,
                bounds.width,
                self.row_height,
            );
            vertices.extend(create_rect_vertices(
                &highlight_rect,
                highlight_color.to_array(),
            ));
        }

        // Selected-row left accent bar (VS Code style active line indicator)
        if is_selected {
            let accent_bar_width = 4.0;
            let accent_rect = Rect::new(
                bounds.x,
                y - self.row_height / 2.0,
                accent_bar_width,
                self.row_height,
            );
            vertices.extend(create_rect_vertices(
                &accent_rect,
                theme::ACCENT.with_alpha(0.6).to_array(),
            ));
        }

        // HEAD indicator (glow behind node) - draw first so it's behind
        if is_head {
            // Outer glow
            vertices.extend(self.create_circle_vertices(
                x,
                y,
                self.node_radius + 4.0,
                theme::ACCENT.with_alpha(0.25).to_array(),
            ));
            // Inner glow
            vertices.extend(self.create_circle_vertices(
                x,
                y,
                self.node_radius + 2.0,
                theme::ACCENT.with_alpha(0.5).to_array(),
            ));
        }

        // Dark outline for depth (draw before the node)
        vertices.extend(self.create_circle_vertices(
            x,
            y,
            self.node_radius + 1.5,
            theme::BACKGROUND.with_alpha(dim_alpha).to_array(),
        ));

        // Commit node style
        let ring_thickness = self.node_radius * 0.38; // ~2.3px ring width
        if commit.is_synthetic {
            // Synthetic "uncommitted changes" node: orange/amber ring with pulsing center
            let synth_color = theme::STATUS_DIRTY.with_alpha(dim_alpha);
            vertices.extend(self.create_ring_vertices(
                x,
                y,
                self.node_radius,
                ring_thickness,
                synth_color.to_array(),
            ));
            // Warm center fill (not dark like regular commits)
            vertices.extend(self.create_circle_vertices(
                x,
                y,
                self.node_radius - ring_thickness,
                theme::STATUS_DIRTY.with_alpha(0.25 * dim_alpha).to_array(),
            ));
        } else if commit.is_orphaned {
            // Orphaned commit: hollow diamond in purple
            let orphan_color = theme::ORPHAN.with_alpha(dim_alpha);
            vertices.extend(Self::create_diamond_vertices(
                x, y, self.node_radius, orphan_color.to_array(),
            ));
            // Inner diamond cutout for hollow effect
            vertices.extend(Self::create_diamond_vertices(
                x, y, self.node_radius * 0.5, theme::BACKGROUND.with_alpha(dim_alpha).to_array(),
            ));
        } else {
            let node_color = layout.color.with_alpha(dim_alpha);
            if is_merge {
                // Merge commit: solid filled circle (visually distinct from regular commits)
                vertices.extend(self.create_circle_vertices(
                    x,
                    y,
                    self.node_radius,
                    node_color.to_array(),
                ));
            } else {
                // Regular commit: donut ring (colored outer, dark center)
                vertices.extend(self.create_ring_vertices(
                    x,
                    y,
                    self.node_radius,
                    ring_thickness,
                    node_color.to_array(),
                ));
                // Dark center fill
                vertices.extend(self.create_circle_vertices(
                    x,
                    y,
                    self.node_radius - ring_thickness,
                    theme::BACKGROUND.with_alpha(dim_alpha).to_array(),
                ));
            }
        }

        // Highlight background for search matches
        if self.search_bar.is_active() && !self.search_bar.query().is_empty() && is_match {
            let highlight_rect = Rect::new(
                bounds.x,
                y - self.row_height / 2.0,
                bounds.width - scrollbar_width,
                self.row_height,
            );
            vertices.extend(create_rect_vertices(
                &highlight_rect,
                theme::STATUS_CLEAN.with_alpha(0.08).to_array(),
            ));
        }
    }

    /// Create vertices for a filled circle (approximated as triangles)
    fn create_circle_vertices(
        &self,
        cx: f32,
        cy: f32,
        radius: f32,
        color: [f32; 4],
    ) -> Vec<SplineVertex> {
        let mut vertices = Vec::new();
        let segments = 24;

        for i in 0..segments {
            let angle1 = (i as f32 / segments as f32) * std::f32::consts::TAU;
            let angle2 = ((i + 1) as f32 / segments as f32) * std::f32::consts::TAU;

            // Center
            vertices.push(SplineVertex {
                position: [cx, cy],
                color,
            });
            // First edge point
            vertices.push(SplineVertex {
                position: [cx + radius * angle1.cos(), cy + radius * angle1.sin()],
                color,
            });
            // Second edge point
            vertices.push(SplineVertex {
                position: [cx + radius * angle2.cos(), cy + radius * angle2.sin()],
                color,
            });
        }

        vertices
    }

    /// Create vertices for a ring (hollow circle)
    fn create_ring_vertices(
        &self,
        cx: f32,
        cy: f32,
        radius: f32,
        thickness: f32,
        color: [f32; 4],
    ) -> Vec<SplineVertex> {
        let mut vertices = Vec::new();
        let segments = 24;
        let inner_radius = radius - thickness;

        for i in 0..segments {
            let angle1 = (i as f32 / segments as f32) * std::f32::consts::TAU;
            let angle2 = ((i + 1) as f32 / segments as f32) * std::f32::consts::TAU;

            let outer1 = [cx + radius * angle1.cos(), cy + radius * angle1.sin()];
            let outer2 = [cx + radius * angle2.cos(), cy + radius * angle2.sin()];
            let inner1 = [
                cx + inner_radius * angle1.cos(),
                cy + inner_radius * angle1.sin(),
            ];
            let inner2 = [
                cx + inner_radius * angle2.cos(),
                cy + inner_radius * angle2.sin(),
            ];

            // Two triangles for the segment
            vertices.push(SplineVertex {
                position: outer1,
                color,
            });
            vertices.push(SplineVertex {
                position: outer2,
                color,
            });
            vertices.push(SplineVertex {
                position: inner1,
                color,
            });

            vertices.push(SplineVertex {
                position: outer2,
                color,
            });
            vertices.push(SplineVertex {
                position: inner2,
                color,
            });
            vertices.push(SplineVertex {
                position: inner1,
                color,
            });
        }

        vertices
    }

    /// Create vertices for a diamond shape (rotated square) — used for orphaned commit nodes
    fn create_diamond_vertices(
        cx: f32,
        cy: f32,
        radius: f32,
        color: [f32; 4],
    ) -> Vec<SplineVertex> {
        let top = [cx, cy - radius];
        let right = [cx + radius, cy];
        let bottom = [cx, cy + radius];
        let left = [cx - radius, cy];

        vec![
            // Triangle 1: top → right → bottom
            SplineVertex { position: top, color },
            SplineVertex { position: right, color },
            SplineVertex { position: bottom, color },
            // Triangle 2: top → bottom → left
            SplineVertex { position: top, color },
            SplineVertex { position: bottom, color },
            SplineVertex { position: left, color },
        ]
    }

    /// Generate text vertices for commit info, and spline vertices for label pill backgrounds.
    ///
    /// Row layout (GitKraken style): graph lanes | branch/tag pills | identicon/avatar | subject line | time (dimmer, right-aligned)
    /// Labels are rendered right after the graph lanes for maximum visibility.
    pub fn layout_text(
        &mut self,
        text_renderer: &TextRenderer,
        commits: &[CommitInfo],
        bounds: Rect,
        avatar_cache: &mut AvatarCache,
        avatar_renderer: &AvatarRenderer,
    ) -> (Vec<TextVertex>, Vec<SplineVertex>, Vec<TextVertex>) {
        let mut vertices = Vec::new();
        let mut pill_vertices = Vec::new();
        let mut avatar_vertices = Vec::new();
        self.pill_click_targets.clear();
        let header_offset = self.header_offset();
        let line_height = text_renderer.line_height();
        let scrollbar_width = self.scrollbar_width();

        // Graph offset for text - right after the graph column (uses adaptive width)
        let text_x = bounds.x + self.lane_left_pad() + self.adaptive_graph_width + self.lane_width * 1.0;

        // Column layout: fixed-width time column right-aligned (~80px for "12 months ago" etc.)
        let time_col_width: f32 = 80.0;
        let stats_col_width: f32 = 100.0; // "+9999 / -9999" fits comfortably
        let right_margin: f32 = 8.0;
        let col_gap: f32 = 16.0; // wider gap to prevent collision with commit messages
        let time_col_right = bounds.right() - right_margin - scrollbar_width;
        let stats_col_right = time_col_right - time_col_width - col_gap;
        let stats_col_left = stats_col_right - stats_col_width;
        let time_col_left = time_col_right - time_col_width;

        // Branch tip lookup
        let branch_tips_by_oid: HashMap<Oid, Vec<&BranchTip>> = self
            .branch_tips
            .iter()
            .fold(HashMap::new(), |mut acc, tip| {
                acc.entry(tip.oid).or_default().push(tip);
                acc
            });

        // Tag lookup
        let tags_by_oid: HashMap<Oid, Vec<&TagInfo>> =
            self.tags.iter().fold(HashMap::new(), |mut acc, tag| {
                acc.entry(tag.oid).or_default().push(tag);
                acc
            });

        // Worktree lookup: only show WT: pills for CLEAN non-current worktrees on their HEAD.
        // Dirty worktrees get their WT: pill on the synthetic row instead.
        let dirty_wt_names: HashSet<String> = commits.iter()
            .filter(|c| c.is_synthetic)
            .filter_map(|c| c.synthetic_wt_name.clone())
            .collect();
        let worktrees_by_oid: HashMap<Oid, Vec<&WorktreeInfo>> = self
            .worktrees
            .iter()
            .filter(|wt| !wt.is_current && !dirty_wt_names.contains(&wt.name))
            .filter_map(|wt| wt.head_oid.map(|oid| (oid, wt)))
            .fold(HashMap::new(), |mut acc, (oid, wt)| {
                acc.entry(oid).or_default().push(wt);
                acc
            });

        // Pre-compute abbreviated worktree names if enabled
        let wt_display_names: HashMap<String, String> = if self.abbreviate_worktree_names && self.worktrees.len() >= 2 {
            let names: Vec<String> = self.worktrees.iter().map(|wt| wt.name.clone()).collect();
            let abbreviated = compute_display_names(&names);
            names.into_iter().zip(abbreviated).collect()
        } else {
            HashMap::new()
        };

        let pill_params = PillParams {
            char_width: text_renderer.char_width(),
            line_height,
            pill_pad_h: 7.0,
            pill_pad_v: 2.0,
            pill_radius: 3.0,
            pill_border_thickness: 1.0,
            col_gap,
            time_col_left,
        };

        for (row, commit) in commits.iter().enumerate() {
            let Some(_layout) = self.layout.get(&commit.id) else {
                continue;
            };

            // row_y returns the center of the row; offset text to center it vertically
            let y = self.row_y(row, &bounds, header_offset) - line_height / 2.0;

            // Skip if outside visible bounds (10-row buffer for smooth scrolling)
            let text_buffer = self.row_height * 10.0;
            if y < bounds.y - text_buffer || y > bounds.bottom() + text_buffer {
                continue;
            }

            let is_head = self.head_oid == Some(commit.id);
            let is_selected = self.selected_commit == Some(commit.id);
            let is_match = self.is_search_match_idx(row);
            let dim_alpha = if is_match { 1.0 } else { 0.2 };

            if commit.is_synthetic {
                // === Synthetic "uncommitted changes" row ===
                let mut current_x = text_x;
                let synth_color = theme::STATUS_DIRTY;

                // WT: pill if this synthetic is for a named worktree
                if let Some(ref wt_name) = commit.synthetic_wt_name {
                    let display_name = wt_display_names.get(wt_name).unwrap_or(wt_name);
                    let wt_label = format!("WT:{}", display_name);
                    let wt_width = text_renderer.measure_text(&wt_label);
                    if current_x + wt_width + pill_params.pill_pad_h * 2.0 + pill_params.char_width
                        <= pill_params.time_col_left - pill_params.col_gap
                    {
                        let pill_rect = Rect::new(
                            current_x,
                            y - pill_params.pill_pad_v,
                            wt_width + pill_params.pill_pad_h * 2.0,
                            line_height + pill_params.pill_pad_v * 2.0,
                        );
                        let wt_text_color = Color::rgba(1.0, 0.596, 0.0, 1.0); // #FF9800
                        pill_vertices.extend(create_rounded_rect_vertices(
                            &pill_rect,
                            Color::rgba(1.0, 0.596, 0.0, 0.20).to_array(),
                            pill_params.pill_radius,
                        ));
                        pill_vertices.extend(create_rounded_rect_outline_vertices(
                            &pill_rect,
                            wt_text_color.with_alpha(0.45).to_array(),
                            pill_params.pill_radius,
                            pill_params.pill_border_thickness,
                        ));
                        vertices.extend(text_renderer.layout_text(
                            &wt_label,
                            current_x + pill_params.pill_pad_h,
                            y,
                            wt_text_color.with_alpha(dim_alpha).to_array(),
                        ));
                        // Track click target for worktree switching
                        self.pill_click_targets.push(PillClickTarget {
                            worktree_name: wt_name.clone(),
                            bounds: [pill_rect.x, pill_rect.y, pill_rect.width, pill_rect.height],
                        });
                        current_x += wt_width + pill_params.pill_pad_h * 2.0 + pill_params.char_width;
                    }
                }

                // Summary text (amber)
                let available = stats_col_left - col_gap - current_x;
                let summary = truncate_to_width(&commit.summary, text_renderer, available);
                vertices.extend(text_renderer.layout_text(
                    &summary,
                    current_x,
                    y,
                    synth_color.with_alpha(0.9 * dim_alpha).to_array(),
                ));

                // Time column for synthetic rows too
                let time_str = commit.relative_time();
                let time_width = text_renderer.measure_text_scaled(&time_str, 0.85);
                let time_x = time_col_right - time_width;
                let time_y = y + (line_height - text_renderer.line_height_small()) * 0.5;
                vertices.extend(text_renderer.layout_text_small(
                    &time_str,
                    time_x,
                    time_y,
                    synth_color.with_alpha(0.5 * dim_alpha).to_array(),
                ));
            } else {
                // === Regular commit row ===

                // === Right-aligned time column (small text) ===
                let time_str = commit.relative_time();
                let time_width = text_renderer.measure_text_scaled(&time_str, 0.85);
                let time_x = time_col_right - time_width;
                // Vertically center the smaller text within the row
                let time_y = y + (line_height - text_renderer.line_height_small()) * 0.5;
                vertices.extend(text_renderer.layout_text_small(
                    &time_str,
                    time_x,
                    time_y,
                    theme::TEXT_MUTED.with_alpha(dim_alpha).to_array(),
                ));

                // === Start rendering from text_x: pills first, then avatar, then subject ===
                let mut current_x = text_x;

                // === Branch and tag pills ===
                current_x = Self::render_branch_pills(
                    text_renderer, &pill_params, y,
                    branch_tips_by_oid.get(&commit.id),
                    current_x, &mut vertices, &mut pill_vertices,
                );
                current_x = Self::render_tag_pills(
                    text_renderer, &pill_params, y,
                    tags_by_oid.get(&commit.id),
                    current_x, &mut vertices, &mut pill_vertices,
                );

                // === Worktree pills (only for clean worktrees — dirty ones show on synthetic row) ===
                let (new_x, wt_targets) = Self::render_worktree_pills(
                    text_renderer, &pill_params, y,
                    worktrees_by_oid.get(&commit.id),
                    current_x, &mut vertices, &mut pill_vertices,
                    &wt_display_names,
                );
                current_x = new_x;
                self.pill_click_targets.extend(wt_targets);

                // === HEAD indicator (after branch/tag/worktree pills) ===
                if is_head && !branch_tips_by_oid.contains_key(&commit.id) {
                    current_x = Self::render_head_pill(
                        text_renderer, &pill_params, y,
                        current_x, &mut vertices, &mut pill_vertices,
                    );
                }

                // === ORPHAN pill for orphaned commits ===
                if commit.is_orphaned {
                    current_x = Self::render_orphan_pill(
                        text_renderer, &pill_params, y,
                        current_x, &mut vertices, &mut pill_vertices,
                    );
                }

                // === Author avatar or identicon fallback (after pills) ===
                current_x = self.render_author_avatar(
                    text_renderer, commit, y, dim_alpha, current_x,
                    avatar_cache, avatar_renderer,
                    &mut vertices, &mut pill_vertices, &mut avatar_vertices,
                );

                // === Diff stats (+N / -M) right-aligned in the stats column ===
                Self::render_diff_stats(
                    text_renderer, commit, y, dim_alpha,
                    stats_col_right, &mut vertices,
                );

                // === Subject line (primary content, bright text, in remaining space) ===
                Self::render_subject_and_body(
                    text_renderer, commit, y, dim_alpha, is_head, is_selected,
                    current_x, stats_col_left - col_gap, &mut vertices,
                );
            }
        }

        // Render search bar text overlay
        if self.search_bar.is_active() {
            let search_bar_pad = self.search_bar_pad();
            let search_bounds = Rect::new(
                bounds.x + search_bar_pad,
                bounds.y + 4.0,
                bounds.width - search_bar_pad * 2.0 - scrollbar_width,
                self.search_bar_height(),
            );
            let search_output = self.search_bar.layout(text_renderer, search_bounds);
            vertices.extend(search_output.text_vertices);
        }

        (vertices, pill_vertices, avatar_vertices)
    }

    /// Render branch label pills (local, remote, HEAD). Returns updated current_x.
    fn render_branch_pills(
        text_renderer: &TextRenderer,
        p: &PillParams,
        y: f32,
        tips: Option<&Vec<&BranchTip>>,
        mut current_x: f32,
        vertices: &mut Vec<TextVertex>,
        pill_vertices: &mut Vec<SplineVertex>,
    ) -> f32 {
        let Some(tips) = tips else { return current_x };
        for tip in tips {
            let (label_color, pill_bg) = if tip.is_remote {
                // Remote tracking: teal/cyan
                (
                    Color::rgba(0.149, 0.776, 0.855, 1.0),  // #26C6DA
                    Color::rgba(0.149, 0.776, 0.855, 0.20),
                )
            } else if tip.is_head {
                // HEAD pointer: green
                (
                    Color::rgba(0.400, 0.733, 0.416, 1.0),  // #66BB6A
                    Color::rgba(0.400, 0.733, 0.416, 0.22),
                )
            } else {
                // Local branches: blue
                (
                    Color::rgba(0.259, 0.647, 0.961, 1.0),  // #42A5F5
                    Color::rgba(0.259, 0.647, 0.961, 0.22),
                )
            };

            let label = &tip.name;
            let label_width = text_renderer.measure_text(label);

            // Don't render if it would overflow into time column
            if current_x + label_width + p.pill_pad_h * 2.0 + p.char_width > p.time_col_left - p.col_gap {
                break;
            }

            // Pill background (rounded rect)
            let pill_rect = Rect::new(
                current_x,
                y - p.pill_pad_v,
                label_width + p.pill_pad_h * 2.0,
                p.line_height + p.pill_pad_v * 2.0,
            );
            pill_vertices.extend(create_rounded_rect_vertices(
                &pill_rect,
                pill_bg.to_array(),
                p.pill_radius,
            ));
            // Pill border outline
            pill_vertices.extend(create_rounded_rect_outline_vertices(
                &pill_rect,
                label_color.with_alpha(0.45).to_array(),
                p.pill_radius,
                p.pill_border_thickness,
            ));

            // Label text (centered in pill)
            vertices.extend(text_renderer.layout_text(
                label,
                current_x + p.pill_pad_h,
                y,
                label_color.to_array(),
            ));
            current_x += label_width + p.pill_pad_h * 2.0 + p.char_width * 1.0;
        }
        current_x
    }

    /// Render tag label pills. Returns updated current_x.
    fn render_tag_pills(
        text_renderer: &TextRenderer,
        p: &PillParams,
        y: f32,
        tags: Option<&Vec<&TagInfo>>,
        mut current_x: f32,
        vertices: &mut Vec<TextVertex>,
        pill_vertices: &mut Vec<SplineVertex>,
    ) -> f32 {
        let Some(tags) = tags else { return current_x };
        for tag in tags {
            let tag_label = format!("\u{25C6} {}", tag.name);
            let tag_width = text_renderer.measure_text(&tag_label);
            if current_x + tag_width + p.pill_pad_h * 2.0 + p.char_width > p.time_col_left - p.col_gap {
                break;
            }
            let pill_rect = Rect::new(
                current_x,
                y - p.pill_pad_v,
                tag_width + p.pill_pad_h * 2.0,
                p.line_height + p.pill_pad_v * 2.0,
            );
            let tag_text_color = Color::rgba(1.0, 0.718, 0.302, 1.0); // #FFB74D
            // Tags: amber/yellow
            pill_vertices.extend(create_rounded_rect_vertices(
                &pill_rect,
                Color::rgba(1.0, 0.718, 0.302, 0.20).to_array(),  // #FFB74D bg
                p.pill_radius,
            ));
            // Tag pill border outline
            pill_vertices.extend(create_rounded_rect_outline_vertices(
                &pill_rect,
                tag_text_color.with_alpha(0.45).to_array(),
                p.pill_radius,
                p.pill_border_thickness,
            ));
            vertices.extend(text_renderer.layout_text(
                &tag_label,
                current_x + p.pill_pad_h,
                y,
                tag_text_color.to_array(),
            ));
            current_x += tag_width + p.pill_pad_h * 2.0 + p.char_width * 1.0;
        }
        current_x
    }

    /// Render worktree label pills (WT:name). Returns (updated current_x, click targets).
    fn render_worktree_pills(
        text_renderer: &TextRenderer,
        p: &PillParams,
        y: f32,
        wts: Option<&Vec<&WorktreeInfo>>,
        mut current_x: f32,
        vertices: &mut Vec<TextVertex>,
        pill_vertices: &mut Vec<SplineVertex>,
        wt_display_names: &HashMap<String, String>,
    ) -> (f32, Vec<PillClickTarget>) {
        let mut click_targets = Vec::new();
        let Some(wts) = wts else { return (current_x, click_targets) };
        for wt in wts {
            let display_name = wt_display_names.get(&wt.name).unwrap_or(&wt.name);
            let wt_label = format!("WT:{}", display_name);
            let wt_width = text_renderer.measure_text(&wt_label);
            if current_x + wt_width + p.pill_pad_h * 2.0 + p.char_width > p.time_col_left - p.col_gap {
                break;
            }
            let pill_rect = Rect::new(
                current_x,
                y - p.pill_pad_v,
                wt_width + p.pill_pad_h * 2.0,
                p.line_height + p.pill_pad_v * 2.0,
            );
            // Track click target for this worktree pill
            click_targets.push(PillClickTarget {
                worktree_name: wt.name.clone(),
                bounds: [pill_rect.x, pill_rect.y, pill_rect.width, pill_rect.height],
            });
            let wt_text_color = Color::rgba(1.0, 0.596, 0.0, 1.0); // #FF9800
            // Worktrees: orange
            pill_vertices.extend(create_rounded_rect_vertices(
                &pill_rect,
                Color::rgba(1.0, 0.596, 0.0, 0.20).to_array(),  // #FF9800 bg
                p.pill_radius,
            ));
            // Worktree pill border outline
            pill_vertices.extend(create_rounded_rect_outline_vertices(
                &pill_rect,
                wt_text_color.with_alpha(0.45).to_array(),
                p.pill_radius,
                p.pill_border_thickness,
            ));
            vertices.extend(text_renderer.layout_text(
                &wt_label,
                current_x + p.pill_pad_h,
                y,
                wt_text_color.to_array(),
            ));
            current_x += wt_width + p.pill_pad_h * 2.0 + p.char_width * 1.0;
        }
        (current_x, click_targets)
    }

    /// Render the HEAD pill (shown when no branch tip points to HEAD). Returns updated current_x.
    fn render_head_pill(
        text_renderer: &TextRenderer,
        p: &PillParams,
        y: f32,
        mut current_x: f32,
        vertices: &mut Vec<TextVertex>,
        pill_vertices: &mut Vec<SplineVertex>,
    ) -> f32 {
        let head_label = "HEAD";
        let head_width = text_renderer.measure_text(head_label);
        if current_x + head_width + p.pill_pad_h * 2.0 < p.time_col_left - p.col_gap {
            let pill_rect = Rect::new(
                current_x,
                y - p.pill_pad_v,
                head_width + p.pill_pad_h * 2.0,
                p.line_height + p.pill_pad_v * 2.0,
            );
            let head_color = Color::rgba(0.400, 0.733, 0.416, 1.0); // #66BB6A
            // HEAD pill: green
            pill_vertices.extend(create_rounded_rect_vertices(
                &pill_rect,
                Color::rgba(0.400, 0.733, 0.416, 0.22).to_array(),  // #66BB6A bg
                p.pill_radius,
            ));
            // HEAD pill border outline
            pill_vertices.extend(create_rounded_rect_outline_vertices(
                &pill_rect,
                head_color.with_alpha(0.45).to_array(),
                p.pill_radius,
                p.pill_border_thickness,
            ));
            vertices.extend(text_renderer.layout_text(
                head_label,
                current_x + p.pill_pad_h,
                y,
                head_color.to_array(),
            ));
            current_x += head_width + p.pill_pad_h * 2.0 + p.char_width * 1.0;
        }
        current_x
    }

    /// Render ORPHAN pill for orphaned commits. Returns updated current_x.
    fn render_orphan_pill(
        text_renderer: &TextRenderer,
        p: &PillParams,
        y: f32,
        mut current_x: f32,
        vertices: &mut Vec<TextVertex>,
        pill_vertices: &mut Vec<SplineVertex>,
    ) -> f32 {
        let label = "ORPHAN";
        let label_width = text_renderer.measure_text(label);
        if current_x + label_width + p.pill_pad_h * 2.0 < p.time_col_left - p.col_gap {
            let pill_rect = Rect::new(
                current_x,
                y - p.pill_pad_v,
                label_width + p.pill_pad_h * 2.0,
                p.line_height + p.pill_pad_v * 2.0,
            );
            pill_vertices.extend(create_rounded_rect_vertices(
                &pill_rect,
                theme::ORPHAN.with_alpha(0.22).to_array(),
                p.pill_radius,
            ));
            pill_vertices.extend(create_rounded_rect_outline_vertices(
                &pill_rect,
                theme::ORPHAN.with_alpha(0.45).to_array(),
                p.pill_radius,
                p.pill_border_thickness,
            ));
            vertices.extend(text_renderer.layout_text(
                label,
                current_x + p.pill_pad_h,
                y,
                theme::ORPHAN.to_array(),
            ));
            current_x += label_width + p.pill_pad_h * 2.0 + p.char_width;
        }
        current_x
    }

    /// Render author avatar or identicon fallback. Returns updated current_x.
    #[allow(clippy::too_many_arguments)]
    fn render_author_avatar(
        &self,
        text_renderer: &TextRenderer,
        commit: &CommitInfo,
        y: f32,
        dim_alpha: f32,
        current_x: f32,
        avatar_cache: &mut AvatarCache,
        avatar_renderer: &AvatarRenderer,
        vertices: &mut Vec<TextVertex>,
        pill_vertices: &mut Vec<SplineVertex>,
        avatar_vertices: &mut Vec<TextVertex>,
    ) -> f32 {
        let line_height = text_renderer.line_height();
        let identicon_radius = (line_height * 0.42).max(5.0);
        let avatar_size = identicon_radius * 2.0;
        let identicon_cx = current_x + identicon_radius;
        let identicon_cy = y + line_height / 2.0;

        // Request avatar download if not already requested
        avatar_cache.request_avatar(&commit.author_email);

        let mut drew_avatar = false;
        if let Some(tex_coords) = avatar_renderer.get_tex_coords(&commit.author_email) {
            // Draw avatar quad
            let ax = identicon_cx - identicon_radius;
            let ay = identicon_cy - identicon_radius;
            let mut quad = avatar::avatar_quad(ax, ay, avatar_size, tex_coords);
            // Apply dim alpha
            for v in &mut quad {
                v.color[3] = dim_alpha;
            }
            avatar_vertices.extend_from_slice(&quad);
            drew_avatar = true;
        }

        if !drew_avatar {
            // Identicon fallback: colored circle with initial
            let identicon_color_idx = author_color_index(&commit.author);
            let identicon_color = IDENTICON_COLORS[identicon_color_idx].with_alpha(dim_alpha);

            pill_vertices.extend(self.create_circle_vertices(
                identicon_cx,
                identicon_cy,
                identicon_radius,
                identicon_color.to_array(),
            ));

            let initial = commit.author.chars().next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_else(|| "?".to_string());
            let initial_width = text_renderer.measure_text(&initial);
            vertices.extend(text_renderer.layout_text(
                &initial,
                identicon_cx - initial_width / 2.0,
                identicon_cy - line_height / 2.0,
                theme::TEXT_BRIGHT.with_alpha(dim_alpha).to_array(),
            ));
        }

        let identicon_advance = identicon_radius * 2.0 + 6.0;
        current_x + identicon_advance
    }

    /// Render diff stats (+N / -M) right-aligned in the stats column
    fn render_diff_stats(
        text_renderer: &TextRenderer,
        commit: &CommitInfo,
        y: f32,
        dim_alpha: f32,
        stats_col_right: f32,
        vertices: &mut Vec<TextVertex>,
    ) {
        if commit.insertions == 0 && commit.deletions == 0 {
            return;
        }
        let line_height = text_renderer.line_height();
        let ins_str = format!("+{}", commit.insertions);
        let del_str = format!("-{}", commit.deletions);
        let sep = " / ";
        let ins_w = text_renderer.measure_text_scaled(&ins_str, 0.85);
        let del_w = text_renderer.measure_text_scaled(&del_str, 0.85);
        let sep_w = text_renderer.measure_text_scaled(sep, 0.85);
        let total_w = ins_w + sep_w + del_w;
        let stats_x = stats_col_right - total_w;
        let stats_y = y + (line_height - text_renderer.line_height_small()) * 0.5;

        // Green insertions
        vertices.extend(text_renderer.layout_text_small(
            &ins_str,
            stats_x,
            stats_y,
            theme::STATUS_CLEAN.with_alpha(0.85 * dim_alpha).to_array(),
        ));
        // Separator
        vertices.extend(text_renderer.layout_text_small(
            sep,
            stats_x + ins_w,
            stats_y,
            theme::TEXT_MUTED.with_alpha(0.5 * dim_alpha).to_array(),
        ));
        // Red deletions
        vertices.extend(text_renderer.layout_text_small(
            &del_str,
            stats_x + ins_w + sep_w,
            stats_y,
            theme::STATUS_DIRTY.with_alpha(0.85 * dim_alpha).to_array(),
        ));
    }

    /// Render commit subject line and optional body excerpt
    #[allow(clippy::too_many_arguments)]
    fn render_subject_and_body(
        text_renderer: &TextRenderer,
        commit: &CommitInfo,
        y: f32,
        dim_alpha: f32,
        is_head: bool,
        is_selected: bool,
        current_x: f32,
        subject_right: f32,
        vertices: &mut Vec<TextVertex>,
    ) {
        let available_width = subject_right - current_x;
        let char_width = text_renderer.char_width();
        let summary_color = if is_selected {
            theme::TEXT_BRIGHT
        } else if commit.is_orphaned {
            theme::TEXT_MUTED.with_alpha(dim_alpha)
        } else if is_head {
            theme::TEXT_BRIGHT.with_alpha(dim_alpha)
        } else {
            theme::TEXT.with_alpha(dim_alpha)
        };

        // Check if we have body text and enough space to show it
        let summary_full_width = text_renderer.measure_text(&commit.summary);
        if summary_full_width <= available_width {
            // Subject fits -- render it in full, then try to append body excerpt
            vertices.extend(text_renderer.layout_text(
                &commit.summary,
                current_x,
                y,
                summary_color.to_array(),
            ));

            if let Some(body) = &commit.body_excerpt {
                let separator = " \u{2014} "; // " -- "
                let sep_width = text_renderer.measure_text(separator);
                let remaining = available_width - summary_full_width - sep_width;
                if remaining > char_width * 5.0 {
                    let body_text = truncate_to_width(body, text_renderer, remaining);
                    let body_x = current_x + summary_full_width;
                    let body_color = theme::TEXT_MUTED.with_alpha(0.7 * dim_alpha).to_array();
                    vertices.extend(text_renderer.layout_text(
                        separator,
                        body_x,
                        y,
                        body_color,
                    ));
                    vertices.extend(text_renderer.layout_text(
                        &body_text,
                        body_x + sep_width,
                        y,
                        body_color,
                    ));
                }
            }
        } else {
            // Subject too long -- truncate it
            let summary = truncate_to_width(&commit.summary, text_renderer, available_width);
            vertices.extend(text_renderer.layout_text(
                &summary,
                current_x,
                y,
                summary_color.to_array(),
            ));
        }
    }
}

/// Shared pill rendering parameters passed to helper methods
struct PillParams {
    char_width: f32,
    line_height: f32,
    pill_pad_h: f32,
    pill_pad_v: f32,
    pill_radius: f32,
    pill_border_thickness: f32,
    col_gap: f32,
    time_col_left: f32,
}

/// Author identicon colors - distinct hues for visual differentiation
const IDENTICON_COLORS: &[Color] = &[
    Color::rgba(0.906, 0.298, 0.235, 1.0), // Red
    Color::rgba(0.204, 0.659, 0.325, 1.0), // Green
    Color::rgba(0.259, 0.522, 0.957, 1.0), // Blue
    Color::rgba(0.608, 0.349, 0.714, 1.0), // Purple
    Color::rgba(0.953, 0.612, 0.071, 1.0), // Amber
    Color::rgba(0.173, 0.733, 0.706, 1.0), // Teal
    Color::rgba(0.914, 0.392, 0.173, 1.0), // Deep Orange
    Color::rgba(0.463, 0.502, 0.898, 1.0), // Indigo
];

/// Hash an author name to get a deterministic color index
fn author_color_index(author: &str) -> usize {
    let hash: u32 = author.bytes().fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    (hash as usize) % IDENTICON_COLORS.len()
}


