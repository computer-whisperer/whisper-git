use std::collections::HashMap;

use git2::Oid;

use crate::git::{BranchTip, CommitInfo, TagInfo, WorkingDirStatus};
use crate::input::{EventResponse, InputEvent, Key, MouseButton};
use crate::ui::widget::{
    create_dashed_rect_outline_vertices, create_rect_vertices, theme,
};
use crate::ui::{Color, Rect, Spline, SplinePoint, SplineVertex, TextRenderer, TextVertex};

/// Lane colors for visual distinction (from UX spec)
const LANE_COLORS: &[Color] = &[
    Color::rgba(0.231, 0.510, 0.965, 1.0), // Blue - primary branch
    Color::rgba(0.133, 0.773, 0.369, 1.0), // Green - feature branches
    Color::rgba(0.961, 0.620, 0.043, 1.0), // Amber - release branches
    Color::rgba(0.659, 0.333, 0.969, 1.0), // Purple - hotfix branches
    Color::rgba(0.392, 0.455, 0.545, 1.0), // Slate - remote tracking
    Color::rgba(0.4, 0.9, 0.9, 1.0),       // Cyan
    Color::rgba(1.0, 0.5, 0.5, 1.0),       // Red
    Color::rgba(0.7, 0.7, 0.9, 1.0),       // Lavender
];

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
    /// Maximum lane used
    max_lane: usize,
}

impl GraphLayout {
    pub fn new() -> Self {
        Self {
            layouts: HashMap::new(),
            active_lanes: Vec::new(),
            max_lane: 0,
        }
    }

    /// Build layout for a list of commits (should be in topological order)
    pub fn build(&mut self, commits: &[CommitInfo]) {
        self.layouts.clear();
        self.active_lanes.clear();
        self.max_lane = 0;

        // Map from commit ID to its index for quick parent lookup
        let commit_indices: HashMap<Oid, usize> = commits
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id, i))
            .collect();

        for (row, commit) in commits.iter().enumerate() {
            // Find lane for this commit
            let lane = self.find_or_assign_lane(commit, &commit_indices);
            let color = LANE_COLORS[lane % LANE_COLORS.len()];

            self.layouts.insert(commit.id, CommitLayout { lane, row, color });

            // Update active lanes based on parents
            self.update_lanes_for_parents(commit, lane, &commit_indices);
        }
    }

    fn find_or_assign_lane(
        &mut self,
        commit: &CommitInfo,
        commit_indices: &HashMap<Oid, usize>,
    ) -> usize {
        // Check if any active lane is waiting for this commit (as a parent)
        for (lane, occupant) in self.active_lanes.iter().enumerate() {
            if *occupant == Some(commit.id) {
                return lane;
            }
        }

        // Check if first parent already has a lane we can continue
        if let Some(&first_parent) = commit.parent_ids.first() {
            if commit_indices.contains_key(&first_parent) {
                // First parent is in our commit list; try to continue its lane
                for (lane, occupant) in self.active_lanes.iter().enumerate() {
                    if occupant.is_none() {
                        self.active_lanes[lane] = Some(first_parent);
                        self.max_lane = self.max_lane.max(lane);
                        return lane;
                    }
                }
            }
        }

        // Find an empty lane or create a new one
        for (lane, occupant) in self.active_lanes.iter().enumerate() {
            if occupant.is_none() {
                self.max_lane = self.max_lane.max(lane);
                return lane;
            }
        }

        // No empty lane, create new one
        let lane = self.active_lanes.len();
        self.active_lanes.push(None);
        self.max_lane = self.max_lane.max(lane);
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
        } else {
            // First parent continues in the same lane
            let first_parent = commit.parent_ids[0];
            if commit_indices.contains_key(&first_parent) {
                self.active_lanes[commit_lane] = Some(first_parent);
            } else {
                self.active_lanes[commit_lane] = None;
            }

            // Additional parents get new lanes (merge sources)
            for &parent_id in commit.parent_ids.iter().skip(1) {
                if commit_indices.contains_key(&parent_id) {
                    // Find or create a lane for this parent
                    let mut found = false;
                    for occupant in self.active_lanes.iter() {
                        if *occupant == Some(parent_id) {
                            found = true;
                            break;
                        }
                    }

                    if !found {
                        // Assign parent to an empty lane or create new one
                        let mut assigned = false;
                        for (lane, occupant) in self.active_lanes.iter_mut().enumerate() {
                            if lane != commit_lane && occupant.is_none() {
                                *occupant = Some(parent_id);
                                self.max_lane = self.max_lane.max(lane);
                                assigned = true;
                                break;
                            }
                        }
                        if !assigned {
                            let lane = self.active_lanes.len();
                            self.active_lanes.push(Some(parent_id));
                            self.max_lane = self.max_lane.max(lane);
                        }
                    }
                }
            }
        }
    }

    /// Get layout for a commit
    pub fn get(&self, id: &Oid) -> Option<&CommitLayout> {
        self.layouts.get(id)
    }

    /// Get maximum lane used
    pub fn max_lane(&self) -> usize {
        self.max_lane
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
}

impl Default for CommitGraphView {
    fn default() -> Self {
        Self {
            layout: GraphLayout::new(),
            title_color: theme::TEXT_BRIGHT,
            text_color: theme::TEXT,
            line_width: 2.0,        // Thinner lines for tighter density
            lane_width: 22.0,       // Compact lanes (~GitKraken density)
            row_height: 24.0,       // Tighter rows for more visible commits
            node_radius: 5.0,       // Smaller nodes for compact layout
            segments_per_curve: 20, // Smoother curves at smaller size
            selected_commit: None,
            hovered_commit: None,
            working_dir_status: None,
            head_oid: None,
            branch_tips: Vec::new(),
            tags: Vec::new(),
            scroll_offset: 0.0,
        }
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

        match event {
            InputEvent::KeyDown { key, .. } => match key {
                Key::J | Key::Down => {
                    // Move selection down
                    self.move_selection(1, commits);
                    EventResponse::Consumed
                }
                Key::K | Key::Up => {
                    // Move selection up
                    self.move_selection(-1, commits);
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
                if bounds.contains(*x, *y) {
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
                if bounds.contains(*x, *y) {
                    for (row, commit) in commits.iter().enumerate() {
                        let commit_y = self.row_y(row, &bounds, header_offset);
                        if (*y - commit_y).abs() < self.row_height / 2.0 {
                            self.hovered_commit = Some(commit.id);
                            break;
                        }
                    }
                }
                EventResponse::Ignored // Don't consume move events
            }
            InputEvent::Scroll { delta_y, .. } => {
                self.scroll_offset = (self.scroll_offset - delta_y).max(0.0);
                EventResponse::Consumed
            }
            _ => EventResponse::Ignored,
        }
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

    /// Generate spline vertices for branch lines and nodes
    pub fn layout_splines(
        &self,
        text_renderer: &TextRenderer,
        commits: &[CommitInfo],
        bounds: Rect,
    ) -> Vec<SplineVertex> {
        let mut vertices = Vec::new();
        let header_offset = 10.0;

        // Background strip for graph column - subtle elevation
        let graph_bg_width = self.graph_width() + 24.0;
        let graph_bg = Rect::new(bounds.x, bounds.y, graph_bg_width, bounds.height);
        vertices.extend(create_rect_vertices(
            &graph_bg,
            theme::SURFACE.to_array(),
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
                            let mut spline = Spline::new(
                                SplinePoint::new(x, y + self.node_radius),
                                color,
                                self.line_width,
                            );

                            // Control points for smooth curve
                            let mid_y = (y + parent_y) / 2.0;
                            let ctrl1 = SplinePoint::new(x, mid_y);
                            let ctrl2 = SplinePoint::new(parent_x, mid_y);

                            spline.cubic_to(
                                ctrl1,
                                ctrl2,
                                SplinePoint::new(parent_x, parent_y - self.node_radius),
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
                theme::BACKGROUND.to_array(),
            ));

            // Commit node (filled circle, or double ring for merge)
            if is_merge {
                // Outer ring for merge indicator
                vertices.extend(self.create_ring_vertices(
                    x,
                    y,
                    self.node_radius + 3.0,
                    2.0,
                    layout.color.to_array(),
                ));
                // Inner filled circle
                vertices.extend(self.create_circle_vertices(
                    x,
                    y,
                    self.node_radius,
                    layout.color.to_array(),
                ));
            } else {
                // Regular commit: filled circle
                vertices.extend(self.create_circle_vertices(
                    x,
                    y,
                    self.node_radius,
                    layout.color.to_array(),
                ));
            }
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
        let segments = 16;

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
        let segments = 16;
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

    /// Generate text vertices for commit info, and spline vertices for label pill backgrounds
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

        // Graph offset for text
        let text_x = bounds.x + 12.0 + self.graph_width() + 10.0;

        // Column layout: fixed-width columns from the right edge
        let time_col_width: f32 = 80.0;
        let right_margin: f32 = 8.0;
        let time_col_right = bounds.right() - right_margin;
        let author_col_right = time_col_right - time_col_width;

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
        let pill_pad_h: f32 = 4.0;
        let pill_pad_v: f32 = 2.0;

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

            // === Right-aligned time column ===
            let time_str = commit.relative_time();
            let time_width = text_renderer.measure_text(&time_str);
            let time_x = time_col_right - time_width;
            vertices.extend(text_renderer.layout_text(
                &time_str,
                time_x,
                y,
                theme::TEXT_MUTED.to_array(),
            ));

            // === Right-aligned author column ===
            let author_display = truncate_author(&commit.author, 12);
            let author_width = text_renderer.measure_text(&author_display);
            let author_x = author_col_right - author_width;
            vertices.extend(text_renderer.layout_text(
                &author_display,
                author_x,
                y,
                theme::TEXT_MUTED.to_array(),
            ));

            // === SHA column (fixed width) ===
            let mut current_x = text_x;
            let sha_col_end = text_x + 7.0 * char_width + char_width;

            vertices.extend(text_renderer.layout_text(
                &commit.short_id,
                current_x,
                y,
                theme::TEXT_MUTED.to_array(),
            ));
            current_x = sha_col_end;

            // === Message column (flexible, up to author column) ===
            // Reserve space for labels between message and author
            let message_end = author_x - char_width * 2.0;
            let available_width = message_end - current_x;
            let max_chars = ((available_width / char_width) as usize).max(4);
            let summary = if commit.summary.len() > max_chars && max_chars > 3 {
                format!("{}...", &commit.summary[..max_chars.saturating_sub(3)])
            } else {
                commit.summary.clone()
            };

            let summary_color = if is_selected {
                theme::TEXT_BRIGHT
            } else {
                theme::TEXT
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
                    if current_x + label_width + pill_pad_h * 2.0 + char_width > author_x - char_width {
                        break;
                    }

                    // Pill background
                    let pill_rect = Rect::new(
                        current_x,
                        y - pill_pad_v,
                        label_width + pill_pad_h * 2.0,
                        line_height + pill_pad_v * 2.0,
                    );
                    pill_vertices.extend(create_rect_vertices(
                        &pill_rect,
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
                if current_x + head_width + pill_pad_h * 2.0 < author_x - char_width {
                    let pill_rect = Rect::new(
                        current_x,
                        y - pill_pad_v,
                        head_width + pill_pad_h * 2.0,
                        line_height + pill_pad_v * 2.0,
                    );
                    pill_vertices.extend(create_rect_vertices(
                        &pill_rect,
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
                    let tag_label = format!("{}  {}", '\u{25C6}', tag.name);
                    let tag_width = text_renderer.measure_text(&tag_label);
                    if current_x + tag_width + pill_pad_h * 2.0 + char_width > author_x - char_width {
                        break;
                    }
                    let pill_rect = Rect::new(
                        current_x,
                        y - pill_pad_v,
                        tag_width + pill_pad_h * 2.0,
                        line_height + pill_pad_v * 2.0,
                    );
                    pill_vertices.extend(create_rect_vertices(
                        &pill_rect,
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

        (vertices, pill_vertices)
    }
}

/// Truncate author name to first name or max characters
fn truncate_author(author: &str, max_chars: usize) -> String {
    // Try first name only
    let first_name = author.split_whitespace().next().unwrap_or(author);
    if first_name.len() <= max_chars {
        first_name.to_string()
    } else {
        format!("{}...", &first_name[..max_chars.saturating_sub(3)])
    }
}
