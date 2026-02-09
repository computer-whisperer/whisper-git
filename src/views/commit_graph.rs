use std::collections::HashMap;
use std::collections::HashSet;

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

use crate::ui::widget::theme::LANE_COLORS;

/// Actions emitted by the commit graph view
pub enum GraphAction {
    /// Request to load more commits (infinite scroll)
    LoadMore,
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
            let lane = self.find_or_assign_lane(commit, &commit_indices);
            let color = LANE_COLORS[lane % LANE_COLORS.len()];

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
        _commit_indices: &HashMap<Oid, usize>,
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

    /// Get maximum lane index that was active at any point
    pub fn max_lane(&self) -> usize {
        self.max_active_lane
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
    /// Set of commit OIDs that match the current search query
    search_matches: HashSet<Oid>,
    /// Pending action to be consumed by the app
    pending_action: Option<GraphAction>,
    /// Guard to prevent rapid-fire LoadMore requests
    loading_more: bool,
    /// Pre-computed Y offsets for each row (time-based variable spacing)
    row_y_offsets: Vec<f32>,
}

impl Default for CommitGraphView {
    fn default() -> Self {
        Self {
            layout: GraphLayout::new(),
            line_width: 2.0,
            lane_width: 22.0,
            row_height: 24.0,
            node_radius: 5.0,
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
            search_matches: HashSet::new(),
            pending_action: None,
            loading_more: false,
            row_y_offsets: Vec::new(),
        }
    }
}

impl CommitGraphView {
    /// Update layout constants to match the current text renderer metrics.
    /// Call this when the display scale changes or at startup.
    pub fn sync_metrics(&mut self, text_renderer: &TextRenderer) {
        let lh = text_renderer.line_height();
        let s = self.row_scale;
        self.row_height = (lh * 1.8 * s).max(20.0 * s);   // ~28.8px at s=1.0, lh=16
        self.lane_width = (lh * 1.0 * s).max(12.0 * s);    // ~16px (compact lanes)
        self.node_radius = (lh * 0.25 * s).max(3.0 * s);   // ~4px (small nodes)
        self.line_width = (lh * 0.12 * s.sqrt()).max(1.5);  // ~1.9px (thin lines)
    }
}

impl CommitGraphView {
    pub fn new() -> Self {
        Self::default()
    }

    /// Height of the column header row at the top of the graph panel
    fn column_header_height(&self) -> f32 {
        self.row_height
    }

    /// Header offset above graph content, scales with row_height.
    /// Includes the column header row.
    fn header_offset(&self) -> f32 {
        self.row_height * 0.35 + self.column_header_height()
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
    fn compute_row_offsets(&mut self, commits: &[CommitInfo]) {
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
            let gap = min_gap + (max_gap - min_gap) * ratio as f32;
            accumulated += gap;
            self.row_y_offsets.push(accumulated);
        }
    }

    /// Calculate the width needed for the graph portion
    fn graph_width(&self) -> f32 {
        let lanes = (self.layout.max_lane() + 1).max(1);
        let computed = lanes as f32 * self.lane_width + self.lane_width * 0.5;
        // Smaller minimum for compact layout
        computed.max(self.lane_width * 1.5)
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

                items.push(MenuItem::separator());
                items.push(MenuItem::new("Cherry-pick", "cherry_pick"));
                items.push(MenuItem::new("Revert Commit", "revert_commit"));
                items.push(MenuItem::new("Create Branch Here", "create_branch"));
                items.push(MenuItem::new("Create Tag Here", "create_tag"));
                items.push(MenuItem::separator());
                items.push(MenuItem::new("Reset Soft to Here", "reset_soft"));
                items.push(MenuItem::new("Reset Mixed to Here", "reset_mixed"));
                items.push(MenuItem::new("Reset Hard to Here", "reset_hard"));

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
        for commit in commits {
            if commit.summary.to_lowercase().contains(&query_lower)
                || commit.author.to_lowercase().contains(&query_lower)
                || commit.short_id.to_lowercase().contains(&query_lower)
                || commit.id.to_string().to_lowercase().starts_with(&query_lower)
            {
                self.search_matches.insert(commit.id);
            }
        }
        self.search_bar.set_match_count(self.search_matches.len());
    }

    /// Check if a commit matches the current search filter
    fn is_search_match(&self, oid: &Oid) -> bool {
        if !self.search_bar.is_active() || self.search_bar.query().is_empty() {
            return true; // No filter active, everything matches
        }
        self.search_matches.contains(oid)
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

        // Background strip for graph column - subtle elevation
        let graph_bg_width = self.graph_width() + self.lane_width * 1.5;
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

        // === Column header row background ===
        let col_header_h = self.column_header_height();
        let col_header_rect = Rect::new(
            bounds.x,
            bounds.y,
            bounds.width - scrollbar_width,
            col_header_h,
        );
        vertices.extend(create_rect_vertices(
            &col_header_rect,
            theme::SURFACE_RAISED.with_alpha(0.5).to_array(),
        ));
        // Bottom border of column header
        vertices.extend(create_rect_vertices(
            &Rect::new(bounds.x, bounds.y + col_header_h - 1.0, bounds.width - scrollbar_width, 1.0),
            theme::BORDER.to_array(),
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
            let y = self.row_y(row, &bounds, header_offset);
            let buffer = self.row_height * 5.0;
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

            // Skip if outside visible area (5-row buffer for stable connection lines)
            let buffer = self.row_height * 5.0;
            if y < bounds.y - buffer || y > bounds.bottom() + buffer {
                continue;
            }

            // Draw connections to parents
            for &parent_id in commit.parent_ids.iter() {
                if let Some(&parent_row) = commit_indices.get(&parent_id)
                    && let Some(parent_layout) = self.layout.get(&parent_id) {
                        let parent_x = self.lane_x(parent_layout.lane, &bounds);
                        let parent_y = self.row_y(parent_row, &bounds, header_offset);

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

            // Draw commit node
            let is_merge = commit.parent_ids.len() > 1;
            let is_selected = self.selected_commit == Some(commit.id);
            let is_hovered = self.hovered_commit == Some(commit.id);
            let is_head = self.head_oid == Some(commit.id);
            let is_match = self.is_search_match(&commit.id);

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
                self.node_radius + 1.0,
                theme::BACKGROUND.with_alpha(dim_alpha).to_array(),
            ));

            // Commit node (filled circle, or double ring for merge)
            let node_color = layout.color.with_alpha(dim_alpha);
            if is_merge {
                // Outer ring for merge indicator
                vertices.extend(self.create_ring_vertices(
                    x,
                    y,
                    self.node_radius + 2.5,
                    1.5,
                    node_color.to_array(),
                ));
                // Inner filled circle
                vertices.extend(self.create_circle_vertices(
                    x,
                    y,
                    self.node_radius,
                    node_color.to_array(),
                ));
            } else {
                // Regular commit: filled circle
                vertices.extend(self.create_circle_vertices(
                    x,
                    y,
                    self.node_radius,
                    node_color.to_array(),
                ));
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

    /// Generate text vertices for commit info, and spline vertices for label pill backgrounds.
    ///
    /// Row layout (GitKraken style): graph lanes | branch/tag pills | identicon/avatar | subject line | time (dimmer, right-aligned)
    /// Labels are rendered right after the graph lanes for maximum visibility.
    pub fn layout_text(
        &self,
        text_renderer: &TextRenderer,
        commits: &[CommitInfo],
        bounds: Rect,
        avatar_cache: &mut AvatarCache,
        avatar_renderer: &AvatarRenderer,
    ) -> (Vec<TextVertex>, Vec<SplineVertex>, Vec<TextVertex>) {
        let mut vertices = Vec::new();
        let mut pill_vertices = Vec::new();
        let mut avatar_vertices = Vec::new();
        let header_offset = self.header_offset();
        let line_height = text_renderer.line_height();
        let scrollbar_width = self.scrollbar_width();

        // Graph offset for text - right after the graph column
        let text_x = bounds.x + self.lane_left_pad() + self.graph_width() + self.lane_width * 1.0;

        // Column layout: fixed-width time column right-aligned (~80px for "12 months ago" etc.)
        let time_col_width: f32 = 80.0;
        let right_margin: f32 = 8.0;
        let col_gap: f32 = 8.0;
        let time_col_right = bounds.right() - right_margin - scrollbar_width;
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

        // Worktree lookup (non-current only — current worktree HEAD is already shown via branch pills)
        let worktrees_by_oid: HashMap<Oid, Vec<&WorktreeInfo>> = self
            .worktrees
            .iter()
            .filter(|wt| !wt.is_current)
            .filter_map(|wt| wt.head_oid.map(|oid| (oid, wt)))
            .fold(HashMap::new(), |mut acc, (oid, wt)| {
                acc.entry(oid).or_default().push(wt);
                acc
            });

        // Dirty worktree lookup — all worktrees (including current) that have dirty files
        // For the "(Working N)" pill indicators at each dirty worktree's HEAD commit
        let dirty_worktrees_by_oid: HashMap<Oid, Vec<&WorktreeInfo>> = self
            .worktrees
            .iter()
            .filter(|wt| wt.is_dirty && wt.head_oid.is_some())
            .filter_map(|wt| wt.head_oid.map(|oid| (oid, wt)))
            .fold(HashMap::new(), |mut acc, (oid, wt)| {
                acc.entry(oid).or_default().push(wt);
                acc
            });

        // Single-worktree fallback: if no linked worktrees exist, use working_dir_status
        // to show a "Working (N)" pill on the HEAD commit
        let fallback_dirty_count = if self.worktrees.is_empty() {
            self.working_dir_status.as_ref()
                .filter(|s| !s.is_clean())
                .map(|s| s.total_files())
        } else {
            None
        };

        let char_width = text_renderer.char_width();
        let pill_pad_h: f32 = 7.0;
        let pill_pad_v: f32 = 2.0;
        let pill_radius: f32 = 3.0;
        let pill_border_thickness: f32 = 1.0;

        // === Column header labels ===
        {
            let col_header_h = self.column_header_height();
            let header_text_y = bounds.y + (col_header_h - line_height) / 2.0;
            let header_color = theme::TEXT_MUTED.to_array();

            // "GRAPH" label over the graph lanes area
            let graph_label = "GRAPH";
            let graph_label_w = text_renderer.measure_text_scaled(graph_label, 0.85);
            let graph_col_center = bounds.x + self.lane_left_pad() + self.graph_width() / 2.0;
            vertices.extend(text_renderer.layout_text_small(
                graph_label,
                graph_col_center - graph_label_w / 2.0,
                header_text_y,
                header_color,
            ));

            // "BRANCH / TAG" label right after graph column
            vertices.extend(text_renderer.layout_text_small(
                "BRANCH / TAG",
                text_x,
                header_text_y,
                header_color,
            ));

            // "COMMIT MESSAGE" label further right (where subject text starts)
            // Estimate a position past typical pill area
            let msg_label_x = text_x + text_renderer.measure_text_scaled("BRANCH / TAG", 0.85) + char_width * 4.0;
            vertices.extend(text_renderer.layout_text_small(
                "COMMIT MESSAGE",
                msg_label_x,
                header_text_y,
                header_color,
            ));

            // "DATE" label right-aligned in the time column
            let date_label = "DATE";
            let date_label_w = text_renderer.measure_text_scaled(date_label, 0.85);
            vertices.extend(text_renderer.layout_text_small(
                date_label,
                time_col_right - date_label_w,
                header_text_y,
                header_color,
            ));
        }

        for (row, commit) in commits.iter().enumerate() {
            let Some(_layout) = self.layout.get(&commit.id) else {
                continue;
            };

            // row_y returns the center of the row; offset text to center it vertically
            let y = self.row_y(row, &bounds, header_offset) - line_height / 2.0;

            // Skip if outside visible bounds (2-row buffer for smooth scrolling)
            let text_buffer = self.row_height * 2.0;
            if y < bounds.y - text_buffer || y > bounds.bottom() + text_buffer {
                continue;
            }

            let is_head = self.head_oid == Some(commit.id);
            let is_selected = self.selected_commit == Some(commit.id);
            let is_match = self.is_search_match(&commit.id);
            let dim_alpha = if is_match { 1.0 } else { 0.2 };

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

            // === Branch labels with pill backgrounds (GitKraken style: before avatar/subject) ===
            if let Some(tips) = branch_tips_by_oid.get(&commit.id) {
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
                    if current_x + label_width + pill_pad_h * 2.0 + char_width > time_col_left - col_gap {
                        break;
                    }

                    // Pill background (rounded rect)
                    let pill_rect = Rect::new(
                        current_x,
                        y - pill_pad_v,
                        label_width + pill_pad_h * 2.0,
                        line_height + pill_pad_v * 2.0,
                    );
                    pill_vertices.extend(create_rounded_rect_vertices(
                        &pill_rect,
                        pill_bg.to_array(),
                        pill_radius,
                    ));
                    // Pill border outline
                    pill_vertices.extend(create_rounded_rect_outline_vertices(
                        &pill_rect,
                        label_color.with_alpha(0.45).to_array(),
                        pill_radius,
                        pill_border_thickness,
                    ));

                    // Label text (centered in pill)
                    vertices.extend(text_renderer.layout_text(
                        label,
                        current_x + pill_pad_h,
                        y,
                        label_color.to_array(),
                    ));
                    current_x += label_width + pill_pad_h * 2.0 + char_width * 1.0;
                }
            }

            // === Tag labels with pill backgrounds (after branch pills) ===
            if let Some(tags) = tags_by_oid.get(&commit.id) {
                for tag in tags {
                    let tag_label = format!("\u{25C6} {}", tag.name);
                    let tag_width = text_renderer.measure_text(&tag_label);
                    if current_x + tag_width + pill_pad_h * 2.0 + char_width > time_col_left - col_gap {
                        break;
                    }
                    let pill_rect = Rect::new(
                        current_x,
                        y - pill_pad_v,
                        tag_width + pill_pad_h * 2.0,
                        line_height + pill_pad_v * 2.0,
                    );
                    let tag_text_color = Color::rgba(1.0, 0.718, 0.302, 1.0); // #FFB74D
                    // Tags: amber/yellow
                    pill_vertices.extend(create_rounded_rect_vertices(
                        &pill_rect,
                        Color::rgba(1.0, 0.718, 0.302, 0.20).to_array(),  // #FFB74D bg
                        pill_radius,
                    ));
                    // Tag pill border outline
                    pill_vertices.extend(create_rounded_rect_outline_vertices(
                        &pill_rect,
                        tag_text_color.with_alpha(0.45).to_array(),
                        pill_radius,
                        pill_border_thickness,
                    ));
                    vertices.extend(text_renderer.layout_text(
                        &tag_label,
                        current_x + pill_pad_h,
                        y,
                        tag_text_color.to_array(),
                    ));
                    current_x += tag_width + pill_pad_h * 2.0 + char_width * 1.0;
                }
            }

            // === Worktree labels with pill backgrounds (after tag pills) ===
            if let Some(wts) = worktrees_by_oid.get(&commit.id) {
                for wt in wts {
                    let wt_label = format!("WT:{}", wt.name);
                    let wt_width = text_renderer.measure_text(&wt_label);
                    if current_x + wt_width + pill_pad_h * 2.0 + char_width > time_col_left - col_gap {
                        break;
                    }
                    let pill_rect = Rect::new(
                        current_x,
                        y - pill_pad_v,
                        wt_width + pill_pad_h * 2.0,
                        line_height + pill_pad_v * 2.0,
                    );
                    let wt_text_color = Color::rgba(1.0, 0.596, 0.0, 1.0); // #FF9800
                    // Worktrees: orange
                    pill_vertices.extend(create_rounded_rect_vertices(
                        &pill_rect,
                        Color::rgba(1.0, 0.596, 0.0, 0.20).to_array(),  // #FF9800 bg
                        pill_radius,
                    ));
                    // Worktree pill border outline
                    pill_vertices.extend(create_rounded_rect_outline_vertices(
                        &pill_rect,
                        wt_text_color.with_alpha(0.45).to_array(),
                        pill_radius,
                        pill_border_thickness,
                    ));
                    vertices.extend(text_renderer.layout_text(
                        &wt_label,
                        current_x + pill_pad_h,
                        y,
                        wt_text_color.to_array(),
                    ));
                    current_x += wt_width + pill_pad_h * 2.0 + char_width * 1.0;
                }
            }

            // === "Working (N)" pills for dirty worktrees at their HEAD commits ===
            {
                // Collect dirty file counts for this commit
                let mut dirty_counts: Vec<usize> = Vec::new();

                // From worktree info (multi-worktree case)
                if let Some(dirty_wts) = dirty_worktrees_by_oid.get(&commit.id) {
                    for wt in dirty_wts {
                        dirty_counts.push(wt.dirty_file_count);
                    }
                }

                // Fallback for single-worktree repos: show on HEAD commit
                if is_head {
                    if let Some(count) = fallback_dirty_count {
                        dirty_counts.push(count);
                    }
                }

                for count in dirty_counts {
                    let wd_label = format!("Working ({})", count);
                    let wd_width = text_renderer.measure_text(&wd_label);
                    if current_x + wd_width + pill_pad_h * 2.0 + char_width > time_col_left - col_gap {
                        break;
                    }
                    let pill_rect = Rect::new(
                        current_x,
                        y - pill_pad_v,
                        wd_width + pill_pad_h * 2.0,
                        line_height + pill_pad_v * 2.0,
                    );
                    // Red "Working" pill with subtle background and outline
                    pill_vertices.extend(create_rounded_rect_vertices(
                        &pill_rect,
                        theme::STATUS_DIRTY.with_alpha(0.15).to_array(),
                        pill_radius,
                    ));
                    pill_vertices.extend(create_rounded_rect_outline_vertices(
                        &pill_rect,
                        theme::STATUS_DIRTY.with_alpha(0.45).to_array(),
                        pill_radius,
                        pill_border_thickness,
                    ));
                    vertices.extend(text_renderer.layout_text(
                        &wd_label,
                        current_x + pill_pad_h,
                        y,
                        theme::STATUS_DIRTY.to_array(),
                    ));
                    current_x += wd_width + pill_pad_h * 2.0 + char_width * 1.0;
                }
            }

            // === HEAD indicator (after branch/tag/worktree pills) ===
            if is_head && !branch_tips_by_oid.contains_key(&commit.id) {
                let head_label = "HEAD";
                let head_width = text_renderer.measure_text(head_label);
                if current_x + head_width + pill_pad_h * 2.0 < time_col_left - col_gap {
                    let pill_rect = Rect::new(
                        current_x,
                        y - pill_pad_v,
                        head_width + pill_pad_h * 2.0,
                        line_height + pill_pad_v * 2.0,
                    );
                    let head_color = Color::rgba(0.400, 0.733, 0.416, 1.0); // #66BB6A
                    // HEAD pill: green
                    pill_vertices.extend(create_rounded_rect_vertices(
                        &pill_rect,
                        Color::rgba(0.400, 0.733, 0.416, 0.22).to_array(),  // #66BB6A bg
                        pill_radius,
                    ));
                    // HEAD pill border outline
                    pill_vertices.extend(create_rounded_rect_outline_vertices(
                        &pill_rect,
                        head_color.with_alpha(0.45).to_array(),
                        pill_radius,
                        pill_border_thickness,
                    ));
                    vertices.extend(text_renderer.layout_text(
                        head_label,
                        current_x + pill_pad_h,
                        y,
                        head_color.to_array(),
                    ));
                    current_x += head_width + pill_pad_h * 2.0 + char_width * 1.0;
                }
            }

            // === Author avatar or identicon fallback (after pills) ===
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
            current_x += identicon_advance;

            // === Subject line (primary content, bright text, in remaining space) ===
            let available_width = (time_col_left - col_gap) - current_x;
            let summary_color = if is_selected {
                theme::TEXT_BRIGHT
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

/// Truncate text to fit within the given pixel width, appending ellipsis if needed
fn truncate_to_width(text: &str, text_renderer: &TextRenderer, max_width: f32) -> String {
    if max_width <= 0.0 {
        return String::new();
    }
    let full_width = text_renderer.measure_text(text);
    if full_width <= max_width {
        return text.to_string();
    }
    let ellipsis = "\u{2026}";
    let ellipsis_width = text_renderer.measure_text(ellipsis);
    let target_width = max_width - ellipsis_width;
    if target_width <= 0.0 {
        return ellipsis.to_string();
    }
    let mut width = 0.0;
    let mut end = 0;
    for (i, c) in text.char_indices() {
        let cw = text_renderer.measure_text(&text[i..i + c.len_utf8()]);
        if width + cw > target_width {
            break;
        }
        width += cw;
        end = i + c.len_utf8();
    }
    format!("{}{}", &text[..end], ellipsis)
}

