use std::collections::HashMap;

use git2::Oid;

use crate::git::CommitInfo;
use crate::ui::{Color, Rect, SplinePoint, SplineRenderer, SplineVertex, Spline, TextRenderer, TextVertex};

/// Lane colors for visual distinction
const LANE_COLORS: &[[f32; 4]] = &[
    [0.4, 0.7, 1.0, 1.0],   // Blue
    [0.5, 0.9, 0.5, 1.0],   // Green
    [1.0, 0.6, 0.4, 1.0],   // Orange
    [0.9, 0.5, 0.9, 1.0],   // Purple
    [1.0, 0.9, 0.4, 1.0],   // Yellow
    [0.4, 0.9, 0.9, 1.0],   // Cyan
    [1.0, 0.5, 0.5, 1.0],   // Red
    [0.7, 0.7, 0.9, 1.0],   // Lavender
];

/// Layout information for a single commit
#[derive(Clone, Debug)]
pub struct CommitLayout {
    pub lane: usize,
    pub row: usize,
    pub color: [f32; 4],
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

            self.layouts.insert(
                commit.id,
                CommitLayout { lane, row, color },
            );

            // Update active lanes based on parents
            self.update_lanes_for_parents(commit, lane, &commit_indices);
        }
    }

    fn find_or_assign_lane(&mut self, commit: &CommitInfo, commit_indices: &HashMap<Oid, usize>) -> usize {
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
                    for (lane, occupant) in self.active_lanes.iter().enumerate() {
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
}

impl Default for CommitGraphView {
    fn default() -> Self {
        Self {
            layout: GraphLayout::new(),
            title_color: Color::rgba(0.9, 0.9, 0.95, 1.0),
            text_color: Color::rgba(0.7, 0.75, 0.8, 1.0),
            line_width: 2.0,
            lane_width: 20.0,
            row_height: 28.0,
            node_radius: 4.0,
            segments_per_curve: 16,
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
        lanes as f32 * self.lane_width + self.lane_width
    }

    /// Get x position for a lane
    fn lane_x(&self, lane: usize, bounds: &Rect) -> f32 {
        bounds.x + 20.0 + lane as f32 * self.lane_width + self.lane_width / 2.0
    }

    /// Get y position for a row
    fn row_y(&self, row: usize, bounds: &Rect, header_offset: f32) -> f32 {
        bounds.y + header_offset + row as f32 * self.row_height + self.row_height / 2.0
    }

    /// Generate spline vertices for branch lines and nodes
    pub fn layout_splines(&self, commits: &[CommitInfo], bounds: Rect) -> Vec<SplineVertex> {
        let mut vertices = Vec::new();
        let header_offset = 50.0; // Space for title

        // Build index for quick parent lookup
        let commit_indices: HashMap<Oid, usize> = commits
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id, i))
            .collect();

        for (row, commit) in commits.iter().enumerate() {
            let Some(layout) = self.layout.get(&commit.id) else {
                continue;
            };

            let x = self.lane_x(layout.lane, &bounds);
            let y = self.row_y(row, &bounds, header_offset);

            // Draw connections to parents
            for (parent_idx, &parent_id) in commit.parent_ids.iter().enumerate() {
                if let Some(&parent_row) = commit_indices.get(&parent_id) {
                    if let Some(parent_layout) = self.layout.get(&parent_id) {
                        let parent_x = self.lane_x(parent_layout.lane, &bounds);
                        let parent_y = self.row_y(parent_row, &bounds, header_offset);

                        // Use the child's color for the connection
                        let color = layout.color;

                        if layout.lane == parent_layout.lane {
                            // Vertical line - same lane
                            let mut spline = Spline::new(
                                SplinePoint::new(x, y + self.node_radius),
                                color,
                                self.line_width,
                            );
                            spline.line_to(SplinePoint::new(parent_x, parent_y - self.node_radius));
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

                            spline.cubic_to(ctrl1, ctrl2, SplinePoint::new(parent_x, parent_y - self.node_radius));
                            vertices.extend(spline.to_vertices(self.segments_per_curve));
                        }
                    }
                }
            }

            // Draw commit node (small filled circle approximated as a polygon)
            vertices.extend(self.create_circle_vertices(x, y, self.node_radius, layout.color));
        }

        vertices
    }

    /// Create vertices for a filled circle (approximated as triangles)
    fn create_circle_vertices(&self, cx: f32, cy: f32, radius: f32, color: [f32; 4]) -> Vec<SplineVertex> {
        let mut vertices = Vec::new();
        let segments = 12;

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

    /// Generate text vertices for commit info
    pub fn layout_text(
        &self,
        text_renderer: &TextRenderer,
        commits: &[CommitInfo],
        bounds: Rect,
    ) -> Vec<TextVertex> {
        let mut vertices = Vec::new();
        let header_offset = 50.0;
        let line_height = text_renderer.line_height();

        // Title
        vertices.extend(text_renderer.layout_text(
            "Commit Graph",
            bounds.x + 20.0,
            bounds.y + 20.0,
            self.title_color.to_array(),
        ));

        // Graph offset for text
        let text_x = bounds.x + 20.0 + self.graph_width() + 10.0;

        for (row, commit) in commits.iter().enumerate() {
            let Some(layout) = self.layout.get(&commit.id) else {
                continue;
            };

            let y = self.row_y(row, &bounds, header_offset) - line_height / 3.0;

            // Skip if outside bounds
            if y > bounds.bottom() - line_height {
                break;
            }

            // Format: short_id summary
            let text = format!("{} {}", commit.short_id, commit.summary);

            // Truncate if too long
            let available_width = bounds.right() - text_x - 20.0;
            let max_chars = (available_width / 10.0) as usize; // Rough estimate
            let text = if text.len() > max_chars && max_chars > 3 {
                format!("{}...", &text[..max_chars.saturating_sub(3)])
            } else {
                text
            };

            // Use lane color for the commit ID portion, lighter for message
            vertices.extend(text_renderer.layout_text(
                &text,
                text_x,
                y,
                self.text_color.to_array(),
            ));
        }

        vertices
    }
}
