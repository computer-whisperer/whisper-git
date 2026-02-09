use std::collections::HashMap;
use std::collections::HashSet;

use git2::Oid;

use crate::git::{BranchTip, CommitInfo, TagInfo, WorkingDirStatus};
use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{
    create_dashed_rect_outline_vertices, create_rect_vertices, theme,
};
use crate::ui::widgets::context_menu::MenuItem;
use crate::ui::widgets::scrollbar::{Scrollbar, ScrollAction};
use crate::ui::widgets::search_bar::{SearchBar, SearchAction};
use crate::ui::{Color, Rect, Spline, SplinePoint, SplineVertex, TextRenderer, TextVertex};

use crate::ui::widget::theme::LANE_COLORS;

/// Layout information for a single commit
#[derive(Clone, Debug)]
pub struct CommitLayout {
    pub lane: usize,
    pub row: usize,
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

        for (row, commit) in commits.iter().enumerate() {
            // Step 1: Find lane for this commit (may already be reserved)
            let lane = self.find_or_assign_lane(commit, &commit_indices);
            let color = LANE_COLORS[lane % LANE_COLORS.len()];

            self.layouts.insert(commit.id, CommitLayout { lane, row, color });

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
            let already_tracked = self.active_lanes.iter().any(|o| *o == Some(parent_id));
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
    title_color: Color,
    text_color: Color,
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
    /// Scroll offset
    pub scroll_offset: f32,
    /// Scrollbar widget
    pub scrollbar: Scrollbar,
    /// Search bar widget
    pub search_bar: SearchBar,
    /// Set of commit OIDs that match the current search query
    search_matches: HashSet<Oid>,
}

impl Default for CommitGraphView {
    fn default() -> Self {
        Self {
            layout: GraphLayout::new(),
            title_color: theme::TEXT_BRIGHT,
            text_color: theme::TEXT,
            line_width: 2.0,
            lane_width: 22.0,
            row_height: 24.0,
            node_radius: 5.0,
            segments_per_curve: 20,
            selected_commit: None,
            hovered_commit: None,
            working_dir_status: None,
            head_oid: None,
            branch_tips: Vec::new(),
            tags: Vec::new(),
            scroll_offset: 0.0,
            scrollbar: Scrollbar::new(),
            search_bar: SearchBar::new(),
            search_matches: HashSet::new(),
        }
    }
}

impl CommitGraphView {
    /// Update layout constants to match the current text renderer metrics.
    /// Call this when the display scale changes or at startup.
    pub fn sync_metrics(&mut self, text_renderer: &TextRenderer) {
        let lh = text_renderer.line_height();
        self.row_height = (lh * 1.8).max(20.0);
        self.lane_width = (lh * 1.4).max(12.0);
        self.node_radius = (lh * 0.38).max(4.0);
        self.line_width = (lh * 0.14).max(1.5);
    }
}

impl CommitGraphView {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update layout for the given commits
    pub fn update_layout(&mut self, commits: &[CommitInfo]) {
        self.layout.build(commits);
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
        bounds.x + 12.0 + lane as f32 * self.lane_width + self.lane_width / 2.0
    }

    /// Get y position for a row (adjusted for scroll and optional working dir node)
    fn row_y(&self, row: usize, bounds: &Rect, header_offset: f32) -> f32 {
        let working_dir_offset = if self.working_dir_status.as_ref().map(|s| !s.is_clean()).unwrap_or(false) {
            self.row_height + 8.0 // Extra space for working dir node
        } else {
            0.0
        };
        bounds.y + header_offset + working_dir_offset + row as f32 * self.row_height
            + self.row_height / 2.0
            - self.scroll_offset
    }

    /// Handle input events
    pub fn handle_event(
        &mut self,
        event: &InputEvent,
        commits: &[CommitInfo],
        bounds: Rect,
    ) -> EventResponse {
        let header_offset = 10.0;
        let scrollbar_width = 10.0;

        // Calculate scrollbar bounds (right edge of graph area)
        let (content_bounds, scrollbar_bounds) = bounds.take_right(scrollbar_width);

        // Search bar bounds (overlay at top of graph area)
        let search_bar_height = 30.0;
        let search_bounds = Rect::new(
            bounds.x + 40.0,
            bounds.y + 4.0,
            bounds.width - 80.0 - scrollbar_width,
            search_bar_height,
        );

        // Handle search bar activation via Ctrl+F or /
        if let InputEvent::KeyDown { key, modifiers, .. } = event {
            if (*key == Key::F && modifiers.only_ctrl()) || (*key == Key::Slash && !modifiers.any() && !self.search_bar.is_active()) {
                self.search_bar.activate();
                return EventResponse::Consumed;
            }
        }

        // Route events to search bar first when active
        if self.search_bar.is_active() {
            if self.search_bar.handle_event(event, search_bounds).is_consumed() {
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
        }

        // Route events to scrollbar
        if self.scrollbar.handle_event(event, scrollbar_bounds).is_consumed() {
            if let Some(ScrollAction::ScrollTo(ratio)) = self.scrollbar.take_action() {
                let max_scroll = (commits.len() as f32 * self.row_height - bounds.height + 60.0).max(0.0);
                self.scroll_offset = (ratio * max_scroll).clamp(0.0, max_scroll);
            }
            return EventResponse::Consumed;
        }

        match event {
            InputEvent::KeyDown { key, .. } => match key {
                Key::J | Key::Down => {
                    // Move selection down
                    self.move_selection(1, commits);
                    self.scroll_to_selection(commits, bounds);
                    EventResponse::Consumed
                }
                Key::K | Key::Up => {
                    // Move selection up
                    self.move_selection(-1, commits);
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
                // Check for click on a commit
                if content_bounds.contains(*x, *y) {
                    for (row, commit) in commits.iter().enumerate() {
                        let commit_y = self.row_y(row, &bounds, header_offset);
                        if (*y - commit_y).abs() < self.row_height / 2.0 {
                            self.selected_commit = Some(commit.id);
                            return EventResponse::Consumed;
                        }
                    }
                }
                EventResponse::Ignored
            }
            InputEvent::MouseMove { x, y, .. } => {
                // Update hover state
                self.hovered_commit = None;
                if content_bounds.contains(*x, *y) {
                    for (row, commit) in commits.iter().enumerate() {
                        let commit_y = self.row_y(row, &bounds, header_offset);
                        if (*y - commit_y).abs() < self.row_height / 2.0 {
                            self.hovered_commit = Some(commit.id);
                            break;
                        }
                    }
                }
                // Also update scrollbar hover
                self.scrollbar.handle_event(event, scrollbar_bounds);
                EventResponse::Ignored // Don't consume move events
            }
            InputEvent::Scroll { delta_y, x, y, .. } => {
                if bounds.contains(*x, *y) {
                    let max_scroll = (commits.len() as f32 * self.row_height - bounds.height + 60.0).max(0.0);
                    self.scroll_offset = (self.scroll_offset - delta_y).max(0.0).min(max_scroll);
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
        let header_offset = 10.0;
        let scrollbar_width = 10.0;
        let (content_bounds, _) = bounds.take_right(scrollbar_width);

        if !content_bounds.contains(x, y) {
            return None;
        }

        for (row, commit) in commits.iter().enumerate() {
            let commit_y = self.row_y(row, &bounds, header_offset);
            if (y - commit_y).abs() < self.row_height / 2.0 {
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

    fn scroll_to_selection(&mut self, commits: &[CommitInfo], bounds: Rect) {
        if let Some(id) = self.selected_commit {
            if let Some(idx) = commits.iter().position(|c| c.id == id) {
                let target_y = idx as f32 * self.row_height;
                let visible_height = bounds.height - 60.0;

                if target_y < self.scroll_offset {
                    self.scroll_offset = target_y;
                } else if target_y > self.scroll_offset + visible_height {
                    self.scroll_offset = target_y - visible_height + self.row_height;
                }
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
        let header_offset = 10.0;
        let scrollbar_width = 10.0;

        // Update scrollbar state
        let visible_rows = (bounds.height / self.row_height).max(1.0) as usize;
        let scroll_offset_items = (self.scroll_offset / self.row_height).round() as usize;
        self.scrollbar.set_content(commits.len(), visible_rows, scroll_offset_items);

        // Background strip for graph column - subtle elevation
        let graph_bg_width = self.graph_width() + 24.0;
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

        // Build index for quick parent lookup
        let commit_indices: HashMap<Oid, usize> = commits
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id, i))
            .collect();

        // Draw working directory node if dirty
        if let Some(ref status) = self.working_dir_status {
            if !status.is_clean() {
                let wd_x = self.lane_x(0, &bounds);
                let wd_y = bounds.y + header_offset + self.row_height / 2.0 - self.scroll_offset;
                let file_count = status.total_files();
                let wd_text = format!("Working ({})", file_count);
                let wd_width = text_renderer.measure_text(&wd_text) + 20.0; // padding
                let wd_height = text_renderer.line_height() + 8.0;

                let wd_rect = Rect::new(wd_x - 10.0, wd_y - wd_height / 2.0, wd_width, wd_height);

                // Subtle background fill
                vertices.extend(create_rect_vertices(
                    &wd_rect,
                    theme::STATUS_DIRTY.with_alpha(0.15).to_array(),
                ));

                // Dashed border for working directory node - thicker
                vertices.extend(create_dashed_rect_outline_vertices(
                    &wd_rect,
                    theme::STATUS_DIRTY.to_array(),
                    2.0,
                    8.0,
                    4.0,
                ));

                // Dashed line to HEAD - thicker and more visible
                if !commits.is_empty() {
                    let head_y = self.row_y(0, &bounds, header_offset);
                    let dash_length = 8.0;
                    let gap_length = 6.0;
                    let total_length = head_y - wd_y - wd_height / 2.0;
                    let num_dashes = (total_length / (dash_length + gap_length)) as i32;

                    for i in 0..num_dashes {
                        let y_start = wd_y + wd_height / 2.0 + i as f32 * (dash_length + gap_length);
                        if y_start + dash_length < head_y - self.node_radius {
                            vertices.extend(create_rect_vertices(
                                &Rect::new(wd_x - 1.5, y_start, 3.0, dash_length),
                                theme::STATUS_DIRTY.with_alpha(0.6).to_array(),
                            ));
                        }
                    }
                }
            }
        }

        for (row, commit) in commits.iter().enumerate() {
            let Some(layout) = self.layout.get(&commit.id) else {
                continue;
            };

            let x = self.lane_x(layout.lane, &bounds);
            let y = self.row_y(row, &bounds, header_offset);

            // Skip if outside visible area
            if y < bounds.y - self.row_height || y > bounds.bottom() + self.row_height {
                continue;
            }

            // Draw connections to parents
            for &parent_id in commit.parent_ids.iter() {
                if let Some(&parent_row) = commit_indices.get(&parent_id) {
                    if let Some(parent_layout) = self.layout.get(&parent_id) {
                        let parent_x = self.lane_x(parent_layout.lane, &bounds);
                        let parent_y = self.row_y(parent_row, &bounds, header_offset);

                        // Use the child's color for the connection
                        let color = layout.color.to_array();

                        if layout.lane == parent_layout.lane {
                            // Vertical line - same lane
                            let mut spline = Spline::new(
                                SplinePoint::new(x, y + self.node_radius),
                                color,
                                self.line_width,
                            );
                            spline.line_to(SplinePoint::new(
                                parent_x,
                                parent_y - self.node_radius,
                            ));
                            vertices.extend(spline.to_vertices(self.segments_per_curve));
                        } else {
                            // Bezier curve - different lanes (merge/fork)
                            let start_y = y + self.node_radius;
                            let end_y = parent_y - self.node_radius;
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
                    theme::SURFACE_HOVER
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
                    self.node_radius + 6.0,
                    theme::ACCENT.with_alpha(0.25).to_array(),
                ));
                // Inner glow
                vertices.extend(self.create_circle_vertices(
                    x,
                    y,
                    self.node_radius + 3.0,
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

            // Commit node (filled circle, or double ring for merge)
            let node_color = layout.color.with_alpha(dim_alpha);
            if is_merge {
                // Outer ring for merge indicator
                vertices.extend(self.create_ring_vertices(
                    x,
                    y,
                    self.node_radius + 3.5,
                    2.5,
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
            let search_bounds = Rect::new(
                bounds.x + 40.0,
                bounds.y + 4.0,
                bounds.width - 80.0 - scrollbar_width,
                30.0,
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
    /// Row layout: graph lanes | subject line | branch/tag labels | author (dimmer) | time (dimmer, right-aligned)
    /// The subject line is the most prominent element (bright text), while author
    /// and time are rendered in muted colors as secondary information.
    pub fn layout_text(
        &self,
        text_renderer: &TextRenderer,
        commits: &[CommitInfo],
        bounds: Rect,
    ) -> (Vec<TextVertex>, Vec<SplineVertex>) {
        let mut vertices = Vec::new();
        let mut pill_vertices = Vec::new();
        let header_offset = 10.0;
        let line_height = text_renderer.line_height();
        let scrollbar_width = 10.0;

        // Graph offset for text - right after the graph column
        let text_x = bounds.x + 12.0 + self.graph_width() + 10.0;

        // Column layout: fixed-width columns from the right edge
        // Keep these compact to maximize subject line space
        let time_col_width: f32 = 64.0;
        let author_col_width: f32 = 80.0;
        let right_margin: f32 = 8.0;
        let col_gap: f32 = 8.0;
        let time_col_right = bounds.right() - right_margin;
        let time_col_left = time_col_right - time_col_width;
        let author_col_right = time_col_left - col_gap;
        let author_col_left = author_col_right - author_col_width;

        // Working directory node text
        if let Some(ref status) = self.working_dir_status {
            if !status.is_clean() {
                let wd_center_y = bounds.y + header_offset + self.row_height / 2.0 - self.scroll_offset;
                let text_y = wd_center_y - line_height / 2.0;
                let file_count = status.total_files();
                let wd_text = format!("Working ({})", file_count);

                vertices.extend(text_renderer.layout_text(
                    &wd_text,
                    self.lane_x(0, &bounds) + 8.0,
                    text_y,
                    theme::STATUS_DIRTY.to_array(),
                ));
            }
        }

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

        let char_width = text_renderer.char_width();
        let pill_pad_h: f32 = 6.0;
        let pill_pad_v: f32 = 2.0;
        let pill_radius: f32 = 3.0;

        for (row, commit) in commits.iter().enumerate() {
            let Some(_layout) = self.layout.get(&commit.id) else {
                continue;
            };

            // row_y returns the center of the row; offset text to center it vertically
            let y = self.row_y(row, &bounds, header_offset) - line_height / 2.0;

            // Skip if outside visible bounds
            if y < bounds.y - line_height || y > bounds.bottom() {
                continue;
            }

            let is_head = self.head_oid == Some(commit.id);
            let is_selected = self.selected_commit == Some(commit.id);
            let is_match = self.is_search_match(&commit.id);
            let dim_alpha = if is_match { 1.0 } else { 0.2 };

            // === Right-aligned time column ===
            let time_str = commit.relative_time();
            let time_width = text_renderer.measure_text(&time_str);
            let time_x = time_col_right - time_width;
            vertices.extend(text_renderer.layout_text(
                &time_str,
                time_x,
                y,
                theme::TEXT_MUTED.with_alpha(dim_alpha).to_array(),
            ));

            // === Right-aligned author column (fixed-width zone) ===
            let author_display = truncate_author(&commit.author, 10);
            let author_width = text_renderer.measure_text(&author_display);
            let author_x = author_col_right - author_width;
            let author_color = if is_selected {
                theme::TEXT
            } else {
                theme::TEXT_MUTED
            };
            vertices.extend(text_renderer.layout_text(
                &author_display,
                author_x,
                y,
                author_color.with_alpha(dim_alpha).to_array(),
            ));

            // === Author identicon (small colored circle with initial) ===
            let identicon_radius = (line_height * 0.42).max(5.0);
            let identicon_cx = text_x + identicon_radius;
            let identicon_cy = y + line_height / 2.0;
            let identicon_color_idx = author_color_index(&commit.author);
            let identicon_color = IDENTICON_COLORS[identicon_color_idx].with_alpha(dim_alpha);

            // Draw filled circle
            pill_vertices.extend(self.create_circle_vertices(
                identicon_cx,
                identicon_cy,
                identicon_radius,
                identicon_color.to_array(),
            ));

            // Draw initial centered in circle
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

            let identicon_advance = identicon_radius * 2.0 + 6.0;

            // === Subject line (primary content, bright text) ===
            let mut current_x = text_x + identicon_advance;
            // The subject occupies the space between graph and the fixed author column
            let available_width = (author_col_left - col_gap) - current_x;
            let max_chars = ((available_width / char_width) as usize).max(4);
            let char_count = commit.summary.chars().count();
            let summary = if char_count > max_chars && max_chars > 3 {
                let truncated: String = commit.summary.chars().take(max_chars.saturating_sub(1)).collect();
                format!("{}\u{2026}", truncated)
            } else {
                commit.summary.clone()
            };

            let summary_color = if is_selected {
                theme::TEXT_BRIGHT
            } else if is_head {
                theme::TEXT_BRIGHT.with_alpha(dim_alpha)
            } else {
                theme::TEXT.with_alpha(dim_alpha)
            };
            vertices.extend(text_renderer.layout_text(
                &summary,
                current_x,
                y,
                summary_color.to_array(),
            ));
            current_x += text_renderer.measure_text(&summary) + char_width;

            // === Branch labels with pill backgrounds ===
            if let Some(tips) = branch_tips_by_oid.get(&commit.id) {
                for tip in tips {
                    let (label_color, pill_bg) = if tip.is_remote {
                        (
                            theme::BRANCH_REMOTE,
                            Color::rgba(0.133, 0.773, 0.369, 0.15),
                        )
                    } else if tip.is_head {
                        (
                            theme::ACCENT,
                            Color::rgba(0.231, 0.510, 0.965, 0.2),
                        )
                    } else {
                        (
                            theme::BRANCH_FEATURE,
                            Color::rgba(0.231, 0.510, 0.965, 0.2),
                        )
                    };

                    let label = &tip.name;
                    let label_width = text_renderer.measure_text(label);

                    // Don't render if it would overlap author column
                    if current_x + label_width + pill_pad_h * 2.0 + char_width > author_col_left - col_gap {
                        break;
                    }

                    // Pill background (rounded rect approximation)
                    let pill_rect = Rect::new(
                        current_x,
                        y - pill_pad_v,
                        label_width + pill_pad_h * 2.0,
                        line_height + pill_pad_v * 2.0,
                    );
                    pill_vertices.extend(self.create_rounded_rect_vertices(
                        &pill_rect,
                        pill_radius,
                        pill_bg.to_array(),
                    ));

                    // Label text (centered in pill)
                    vertices.extend(text_renderer.layout_text(
                        label,
                        current_x + pill_pad_h,
                        y,
                        label_color.to_array(),
                    ));
                    current_x += label_width + pill_pad_h * 2.0 + char_width * 0.5;
                }
            }

            // HEAD indicator
            if is_head && !branch_tips_by_oid.contains_key(&commit.id) {
                let head_label = "HEAD";
                let head_width = text_renderer.measure_text(head_label);
                if current_x + head_width + pill_pad_h * 2.0 < author_col_left - col_gap {
                    let pill_rect = Rect::new(
                        current_x,
                        y - pill_pad_v,
                        head_width + pill_pad_h * 2.0,
                        line_height + pill_pad_v * 2.0,
                    );
                    pill_vertices.extend(self.create_rounded_rect_vertices(
                        &pill_rect,
                        pill_radius,
                        Color::rgba(0.231, 0.510, 0.965, 0.2).to_array(),
                    ));
                    vertices.extend(text_renderer.layout_text(
                        head_label,
                        current_x + pill_pad_h,
                        y,
                        theme::ACCENT.to_array(),
                    ));
                    current_x += head_width + pill_pad_h * 2.0 + char_width * 0.5;
                }
            }

            // Tags with pill backgrounds
            if let Some(tags) = tags_by_oid.get(&commit.id) {
                for tag in tags {
                    let tag_label = format!("\u{25C6} {}", tag.name);
                    let tag_width = text_renderer.measure_text(&tag_label);
                    if current_x + tag_width + pill_pad_h * 2.0 + char_width > author_col_left - col_gap {
                        break;
                    }
                    let pill_rect = Rect::new(
                        current_x,
                        y - pill_pad_v,
                        tag_width + pill_pad_h * 2.0,
                        line_height + pill_pad_v * 2.0,
                    );
                    pill_vertices.extend(self.create_rounded_rect_vertices(
                        &pill_rect,
                        pill_radius,
                        Color::rgba(0.961, 0.620, 0.043, 0.2).to_array(),
                    ));
                    vertices.extend(text_renderer.layout_text(
                        &tag_label,
                        current_x + pill_pad_h,
                        y,
                        theme::BRANCH_RELEASE.to_array(),
                    ));
                    current_x += tag_width + pill_pad_h * 2.0 + char_width * 0.5;
                }
            }
        }

        // Render search bar text overlay
        if self.search_bar.is_active() {
            let search_bounds = Rect::new(
                bounds.x + 40.0,
                bounds.y + 4.0,
                bounds.width - 80.0 - scrollbar_width,
                30.0,
            );
            let search_output = self.search_bar.layout(text_renderer, search_bounds);
            vertices.extend(search_output.text_vertices);
        }

        (vertices, pill_vertices)
    }

    /// Create vertices for a rounded rectangle (pill shape)
    fn create_rounded_rect_vertices(
        &self,
        rect: &Rect,
        radius: f32,
        color: [f32; 4],
    ) -> Vec<SplineVertex> {
        let mut vertices = Vec::new();
        let r = radius.min(rect.width / 2.0).min(rect.height / 2.0);

        // Central rectangle (excluding corners)
        vertices.extend(create_rect_vertices(
            &Rect::new(rect.x + r, rect.y, rect.width - 2.0 * r, rect.height),
            color,
        ));
        // Left strip
        vertices.extend(create_rect_vertices(
            &Rect::new(rect.x, rect.y + r, r, rect.height - 2.0 * r),
            color,
        ));
        // Right strip
        vertices.extend(create_rect_vertices(
            &Rect::new(rect.right() - r, rect.y + r, r, rect.height - 2.0 * r),
            color,
        ));

        // Corner arcs (quarter circles)
        let corners = [
            (rect.x + r, rect.y + r, std::f32::consts::PI, std::f32::consts::FRAC_PI_2 * 3.0),           // top-left
            (rect.right() - r, rect.y + r, std::f32::consts::FRAC_PI_2 * 3.0, std::f32::consts::TAU),    // top-right
            (rect.right() - r, rect.bottom() - r, 0.0, std::f32::consts::FRAC_PI_2),                      // bottom-right
            (rect.x + r, rect.bottom() - r, std::f32::consts::FRAC_PI_2, std::f32::consts::PI),           // bottom-left
        ];

        let segments = 6;
        for (cx, cy, start_angle, end_angle) in corners {
            for i in 0..segments {
                let a1 = start_angle + (end_angle - start_angle) * (i as f32 / segments as f32);
                let a2 = start_angle + (end_angle - start_angle) * ((i + 1) as f32 / segments as f32);
                vertices.push(SplineVertex { position: [cx, cy], color });
                vertices.push(SplineVertex { position: [cx + r * a1.cos(), cy + r * a1.sin()], color });
                vertices.push(SplineVertex { position: [cx + r * a2.cos(), cy + r * a2.sin()], color });
            }
        }

        vertices
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

/// Truncate author name to first name or max characters
fn truncate_author(author: &str, max_chars: usize) -> String {
    // Try first name only
    let first_name = author.split_whitespace().next().unwrap_or(author);
    if first_name.chars().count() <= max_chars {
        first_name.to_string()
    } else {
        let truncated: String = first_name.chars().take(max_chars.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}
