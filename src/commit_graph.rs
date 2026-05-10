//! Commit-graph history view.
//!
//! `GraphLayout` is the lane-assignment algorithm — pure data, no
//! aetna types. It walks topologically-ordered commits and assigns
//! each one a lane index plus inherits the lane palette from
//! [`LANE_COLORS`]. Ported from the pre-aetna `views/commit_graph.rs`
//! with the terminal-graph parts (Bezier merge curves, scrollbar
//! widget, search bar) stripped — Phase 6 paints lane lines per row
//! via the `commit_node` shader and lets aetna own scroll + hit-test.
//!
//! `history_view` composes a `virtual_list_dyn` and per-row `El`. Row
//! heights are time-spaced — bigger time deltas between consecutive
//! commits stretch the row vertically, with the node anchored to a
//! fixed `NODE_Y` offset so labels line up cleanly. The dyn variant
//! measures realised heights on first viewport entry and caches them.

use std::collections::HashMap;

use aetna_core::vector::{PathBuilder, VectorAsset, VectorLineCap, VectorPath};
use aetna_core::widgets::text_input::text_input;
use aetna_core::image::Image;
use aetna_core::{Color, El, Selection, prelude::*};
use git2::Oid;

use crate::ci::{CiState, ProviderCommitRollup};
use crate::git::CommitInfo;
use crate::repo_tab::RepoTab;

pub const ROW_HEIGHT: f32 = 28.0;
/// Per-lane horizontal slot inside the graph cell. Width per lane,
/// in pixels — picks the same density as the pre-port.
pub const LANE_W: f32 = 24.0;
/// Cap on the visible lane count. Beyond this, deeper lanes stack on
/// the rightmost slot — keeps very deep graphs from pushing the
/// commit text off-screen.
pub const LANE_COUNT_VISUAL: u8 = 6;
/// Where in a row's local frame the commit node sits, in pixels from
/// the row's top. Stays fixed even when the row is taller than
/// `ROW_HEIGHT` (time-spaced) so the visual rhythm of "node + label"
/// stays aligned across rows. Extra height (from time spacing) hangs
/// below the node as breathing room before the next commit.
pub const NODE_Y: f32 = ROW_HEIGHT / 2.0;
/// Maximum extra row height added by time spacing on top of the
/// minimum (`ROW_HEIGHT`). The pre-port's `compute_row_offsets` used
/// `max_gap = 2 * row_height`; we mirror that.
const MAX_EXTRA_HEIGHT: f32 = ROW_HEIGHT;
/// Time-delta scale for the log-curve mapping. Two-hour reference
/// keeps small deltas (rapid-fire commits in a sprint) at the minimum
/// height; deltas longer than this start adding visible spacing.
const TIME_BASE_SECONDS: f64 = 7200.0;
/// Time delta past which the log curve saturates at `MAX_EXTRA_HEIGHT`
/// — 30 days. Longer gaps don't make rows any taller.
const TIME_MAX_DELTA_SECONDS: f64 = 30.0 * 24.0 * 3600.0;

/// Lane palette — picks from aetna tokens that are stable across
/// theme swaps. The fixed RGB constants in the original whisper-git
/// palette would have re-introduced theme drift.
pub const LANE_COLORS: [Color; 6] = [
    tokens::PRIMARY,
    tokens::SUCCESS,
    tokens::WARNING,
    tokens::INFO,
    tokens::DESTRUCTIVE,
    tokens::FOREGROUND,
];

/// The orphan color — used when a commit is reachable only through
/// reflogs. Matches the muted text color so orphans visually recede.
pub const ORPHAN_COLOR: Color = tokens::MUTED_FOREGROUND;

/// Identicon palette — 8 distinct colors, picked deterministically
/// per author so the same author always lands on the same color.
/// Matches the pre-port's identicon palette (Material-ish hues that
/// stay legible against either dark or light backgrounds).
const IDENTICON_COLORS: &[Color] = &[
    Color::rgb(231, 76, 60),  // red
    Color::rgb(52, 168, 83),  // green
    Color::rgb(66, 133, 244), // blue
    Color::rgb(155, 89, 182), // purple
    Color::rgb(243, 156, 18), // amber
    Color::rgb(44, 187, 180), // teal
    Color::rgb(233, 100, 44), // deep orange
    Color::rgb(118, 128, 229), // indigo
];

/// Pixel diameter of the author identicon — sized to align with row
/// caption text height.
const AVATAR_SIZE: f32 = 18.0;

/// Height of the pills band that sits above the main content row when
/// a commit carries any branch / tag / worktree / HEAD / ORPHAN /
/// PINNED chrome. Pills themselves are ~20 px tall (caption line
/// height 16 + 2 × 2 px vertical padding); the extra 8 px in the
/// band is intentional slack, bottom-aligned, so the breathing room
/// sits *above* the pills — visually anchoring them to the commit
/// they describe (below) rather than the unrelated commit above.
const PILLS_BAND_HEIGHT: f32 = 28.0;

/// Right-side gutter reserved on focusable rows inside `virtual_list_dyn`
/// so the row's bounding rect — and its focus ring — don't overlap the
/// scrollbar thumb's active track. = `SCROLLBAR_THUMB_WIDTH_ACTIVE`
/// (10) + `SCROLLBAR_TRACK_INSET` (2). Without it every focused row
/// trips the `ScrollbarObscuresFocusable` lint on the right edge.
const SCROLLBAR_GUTTER: f32 =
    tokens::SCROLLBAR_THUMB_WIDTH_ACTIVE + tokens::SCROLLBAR_TRACK_INSET;

/// Hash an author name to a deterministic color slot.
fn author_color(author: &str) -> Color {
    let hash: u32 = author
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    IDENTICON_COLORS[(hash as usize) % IDENTICON_COLORS.len()]
}

/// Author avatar — Gravatar image when one's loaded for this email,
/// identicon fallback otherwise. The identicon is a deterministic-
/// color disc with the author's first letter (uppercase) centered;
/// authors that haven't been hashed yet show as a `?`.
///
/// `gravatar` is `Some` when the avatar cache has finished fetching
/// + decoding for this email; `None` covers in-flight, failed (404),
/// and not-yet-requested. `key` is required so the avatar
/// participates in pointer hit-testing — aetna only fires tooltips on
/// keyed elements.
fn author_avatar(author: &str, gravatar: Option<Image>, key: String) -> El {
    if let Some(img) = gravatar {
        return image(img)
            .width(Size::Fixed(AVATAR_SIZE))
            .height(Size::Fixed(AVATAR_SIZE))
            .radius(AVATAR_SIZE * 0.5)
            .clip()
            .key(key)
            .tooltip(author.to_string());
    }
    let initial = author
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string());
    let color = author_color(author);
    column([text(initial).caption().text_color(tokens::FOREGROUND)])
        .width(Size::Fixed(AVATAR_SIZE))
        .height(Size::Fixed(AVATAR_SIZE))
        .fill(color)
        .radius(AVATAR_SIZE * 0.5)
        .align(Align::Center)
        .justify(Justify::Center)
        .key(key)
        .tooltip(author.to_string())
}

#[derive(Clone, Debug)]
pub struct CommitLayout {
    pub lane: usize,
    pub color: Color,
}

/// One edge from a commit to one of its parents. The child sits at
/// `child_row` on `child_lane`; the parent at `parent_row` on
/// `parent_lane`. `child_row < parent_row` because commits are stored
/// newest-first. The connection inherits the child's lane color, per
/// the pre-port convention.
#[derive(Clone, Debug)]
pub struct GraphEdge {
    pub child_row: usize,
    pub child_lane: usize,
    pub parent_row: usize,
    pub parent_lane: usize,
    pub color: Color,
}

/// One cubic-bezier segment that lives in a single row's vertical
/// strip, expressed in row-local coordinates (y in `[0, ROW_HEIGHT]`).
/// Cross-lane edges contribute one of these per row they pass through;
/// same-lane edges emit `RowGeometry::*_verticals` entries instead.
#[derive(Clone, Debug)]
pub struct CurveSegment {
    pub p0: (f32, f32),
    pub p1: (f32, f32),
    pub p2: (f32, f32),
    pub p3: (f32, f32),
    pub color: Color,
}

/// Per-row paint data — what verticals to draw in this row's strip,
/// what curve segments pass through, and whether the row hosts a node.
/// All coords are row-local (y ∈ [0, height]).
///
/// Verticals come in three flavors so the node circle can interrupt
/// the line cleanly: full-height passes through this row, top-half
/// stops at `NODE_Y` (incoming lane terminating at the node),
/// bottom-half starts at `NODE_Y` (outgoing lane spawning at the node
/// and stretching down to the row's bottom).
///
/// `height` is the row's vertical extent in pixels — driven by
/// time spacing between this commit and the next (older) one.
/// Always >= `ROW_HEIGHT`, integer-rounded for clean MSDF tiling.
#[derive(Clone, Debug)]
pub struct RowGeometry {
    pub height: f32,
    pub full_verticals: Vec<(usize, Color)>,
    pub top_half_verticals: Vec<(usize, Color)>,
    pub bottom_half_verticals: Vec<(usize, Color)>,
    pub curves: Vec<CurveSegment>,
    /// Where the commit node sits within this row's strip, in pixels
    /// from the row's top. Defaults to `NODE_Y`. Pushed down by a
    /// pills band sitting above the main content so the node stays
    /// visually centered on the SHA / message line.
    pub node_y: f32,
}

impl Default for RowGeometry {
    fn default() -> Self {
        Self {
            height: ROW_HEIGHT,
            full_verticals: Vec::new(),
            top_half_verticals: Vec::new(),
            bottom_half_verticals: Vec::new(),
            curves: Vec::new(),
            node_y: NODE_Y,
        }
    }
}

/// Lane-assignment state. Walks commits in topological order, keeping
/// `active_lanes[i] = Some(parent_oid)` for each lane currently
/// "waiting" for that parent to appear. Tip commits land on the lowest
/// free lane; first parents inherit the lane; secondary parents
/// (merges) get fresh lanes.
#[derive(Default)]
pub struct GraphLayout {
    layouts: HashMap<Oid, CommitLayout>,
    active_lanes: Vec<Option<Oid>>,
    pub max_lane: usize,
    /// Edges built alongside lane assignment — one per commit/parent
    /// pair where both ends are in the commit list. Ready to read by
    /// the row-paint pipeline.
    pub edges: Vec<GraphEdge>,
    /// Per-row paint data, indexed by row. `row_geometry[i]` is what
    /// the i-th commit's row should paint in its graph cell.
    pub row_geometry: Vec<RowGeometry>,
    /// Pixel width of the graph column for this commit list — derived
    /// from `max_lane` and capped at `LANE_COUNT_VISUAL` lanes. Linear
    /// histories collapse to a single lane's width; deeper graphs
    /// expand up to the cap. Read once per render and applied to
    /// every row's `vector()` cell so widths stay aligned.
    pub graph_width: f32,
}

impl GraphLayout {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn build(&mut self, commits: &[CommitInfo]) {
        self.layouts.clear();
        self.active_lanes.clear();
        self.max_lane = 0;
        self.edges.clear();
        self.row_geometry.clear();

        let commit_set: HashMap<Oid, ()> = commits.iter().map(|c| (c.id, ())).collect();
        let row_by_oid: HashMap<Oid, usize> =
            commits.iter().enumerate().map(|(i, c)| (c.id, i)).collect();

        for commit in commits {
            let lane = self.find_or_assign_lane(commit);
            let color = if commit.is_orphaned {
                ORPHAN_COLOR
            } else {
                LANE_COLORS[lane % LANE_COLORS.len()]
            };
            self.layouts.insert(commit.id, CommitLayout { lane, color });

            // Free other lanes that were also waiting for this commit
            // (multiple children pointing at the same parent).
            for i in 0..self.active_lanes.len() {
                if i != lane && self.active_lanes[i] == Some(commit.id) {
                    self.active_lanes[i] = None;
                }
            }

            self.update_lanes_for_parents(commit, lane, &commit_set);
            self.update_peak();
        }

        // Lane assignment done — build the edge list now that every
        // commit has a known lane. One edge per (commit, in-list parent)
        // pair; the connection inherits the child's color, matching the
        // pre-port look.
        for (child_row, commit) in commits.iter().enumerate() {
            let Some(child_layout) = self.layouts.get(&commit.id) else {
                continue;
            };
            for parent_id in &commit.parent_ids {
                let Some(&parent_row) = row_by_oid.get(parent_id) else {
                    continue;
                };
                let Some(parent_layout) = self.layouts.get(parent_id) else {
                    continue;
                };
                self.edges.push(GraphEdge {
                    child_row,
                    child_lane: child_layout.lane,
                    parent_row,
                    parent_lane: parent_layout.lane,
                    color: child_layout.color,
                });
            }
        }

        // Per-row heights — time-spaced, integer-rounded. Computed
        // BEFORE edge decomposition so the bezier subdivision uses
        // correct row strips. Each row's height is the time-spaced gap
        // to the next (older) commit; the last row uses ROW_HEIGHT.
        let heights = compute_row_heights(commits);
        // Accumulated absolute Y offsets — `row_top_y[i]` is where
        // row i starts; `row_top_y[i] + heights[i]` is where it ends.
        let mut row_top_y = Vec::with_capacity(commits.len() + 1);
        let mut acc = 0.0f32;
        for &h in &heights {
            row_top_y.push(acc);
            acc += h;
        }
        row_top_y.push(acc);

        self.row_geometry = heights
            .iter()
            .map(|&h| RowGeometry {
                height: h,
                ..Default::default()
            })
            .collect();
        for edge in &self.edges {
            decompose_edge_into_rows(edge, &row_top_y, &mut self.row_geometry);
        }

        // Adaptive column width — narrows for shallow graphs, expands
        // for deeper ones up to the visible cap. `max_lane` is 0 for a
        // linear history (one node-column), so `+1` gives us "lanes
        // actually in use." Capped so very deep graphs don't push the
        // commit text off-screen.
        let visible_lanes = (self.max_lane + 1).min(LANE_COUNT_VISUAL as usize);
        self.graph_width = (visible_lanes as f32 * LANE_W).max(LANE_W);
    }

    pub fn get(&self, id: &Oid) -> Option<&CommitLayout> {
        self.layouts.get(id)
    }

    /// Recompute per-row geometry with a band of extra height above
    /// each row that needs one (e.g., for an above-the-message pills
    /// strip). `band_heights[i]` adds to row i's strip height and
    /// pushes its node down by the same amount, keeping the node
    /// vertically centered on the main content line. When all bands
    /// are zero this matches the geometry produced by `build`.
    pub fn row_geometry_with_bands(
        &self,
        commits: &[CommitInfo],
        band_heights: &[f32],
    ) -> Vec<RowGeometry> {
        let heights = compute_row_heights(commits);
        let zero = 0.0f32;
        let mut row_top_y = Vec::with_capacity(commits.len() + 1);
        let mut acc = 0.0f32;
        for (i, &h) in heights.iter().enumerate() {
            let band = band_heights.get(i).copied().unwrap_or(zero);
            row_top_y.push(acc);
            acc += h + band;
        }
        row_top_y.push(acc);

        let mut geom: Vec<RowGeometry> = heights
            .iter()
            .enumerate()
            .map(|(i, &h)| {
                let band = band_heights.get(i).copied().unwrap_or(zero);
                RowGeometry {
                    height: (h + band).round(),
                    node_y: (band + NODE_Y).round(),
                    ..Default::default()
                }
            })
            .collect();
        for edge in &self.edges {
            decompose_edge_into_rows(edge, &row_top_y, &mut geom);
        }
        geom
    }

    fn find_or_assign_lane(&mut self, commit: &CommitInfo) -> usize {
        for (lane, occupant) in self.active_lanes.iter().enumerate() {
            if *occupant == Some(commit.id) {
                return lane;
            }
        }
        let lane = self.lowest_free_lane();
        while self.active_lanes.len() <= lane {
            self.active_lanes.push(None);
        }
        lane
    }

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

    fn update_lanes_for_parents(
        &mut self,
        commit: &CommitInfo,
        commit_lane: usize,
        commit_set: &HashMap<Oid, ()>,
    ) {
        while self.active_lanes.len() <= commit_lane {
            self.active_lanes.push(None);
        }

        if commit.parent_ids.is_empty() {
            self.active_lanes[commit_lane] = None;
            return;
        }

        let first_parent = commit.parent_ids[0];
        if commit_set.contains_key(&first_parent) {
            self.active_lanes[commit_lane] = Some(first_parent);
        } else {
            self.active_lanes[commit_lane] = None;
        }

        for &parent_id in commit.parent_ids.iter().skip(1) {
            if !commit_set.contains_key(&parent_id) {
                continue;
            }
            if self.active_lanes.contains(&Some(parent_id)) {
                continue;
            }
            let lane = self.lowest_free_lane();
            while self.active_lanes.len() <= lane {
                self.active_lanes.push(None);
            }
            self.active_lanes[lane] = Some(parent_id);
        }
    }

    fn update_peak(&mut self) {
        for (i, occupant) in self.active_lanes.iter().enumerate().rev() {
            if occupant.is_some() {
                if i > self.max_lane {
                    self.max_lane = i;
                }
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-row heights and edge → per-row geometry decomposition
// ---------------------------------------------------------------------------

/// Compute the vertical extent of each row from time spacing between
/// adjacent commits. Each row's height is the gap to the *next*
/// (older) commit; the last row gets the minimum height.
///
/// Mapping: log-curve from the time delta to a height in
/// `[ROW_HEIGHT, ROW_HEIGHT + MAX_EXTRA_HEIGHT]`. Heights are rounded
/// to integer pixels — the `vector_smoke` test confirmed integer +
/// pixel-aligned rows are necessary for clean MSDF tiling.
pub fn compute_row_heights(commits: &[CommitInfo]) -> Vec<f32> {
    if commits.is_empty() {
        return Vec::new();
    }
    let log_max = (1.0 + TIME_MAX_DELTA_SECONDS / TIME_BASE_SECONDS).ln();
    let mut heights = Vec::with_capacity(commits.len());
    for i in 0..commits.len() {
        let h = if i + 1 < commits.len() {
            // Commits are stored newest-first, so `commits[i].time >=
            // commits[i+1].time` is the expected order. `unsigned_abs`
            // keeps a synthetic out-of-order entry from blowing up.
            let delta = (commits[i].time - commits[i + 1].time).unsigned_abs() as f64;
            let clamped = delta.min(TIME_MAX_DELTA_SECONDS);
            let ratio = (1.0 + clamped / TIME_BASE_SECONDS).ln() / log_max;
            ROW_HEIGHT + MAX_EXTRA_HEIGHT * ratio as f32
        } else {
            ROW_HEIGHT
        };
        heights.push(h.round());
    }
    heights
}

/// Decompose one edge into per-row paint entries.
///
/// Same-lane edges become a sequence of verticals (bottom-half on the
/// child's row, full-height on intermediate rows, top-half on the
/// parent's row). Cross-lane edges become a single full-distance cubic
/// bezier from `(child_lane, child_node_y)` to `(parent_lane,
/// parent_node_y)`, subdivided so each spanned row gets its own
/// row-local segment. The control-point recipe (`start_y + dy*0.4`,
/// `end_y - dy*0.4`) mirrors the pre-port `render_graph_connections`
/// so the curve shape is the same — a long S that hugs the child's
/// lane near the top, transitions in the middle, then settles into
/// the parent's lane near the bottom.
///
/// `row_top_y[i]` is the absolute Y of row `i`'s top edge; the last
/// entry is the total content height (so `row_top_y[i+1] - row_top_y[i]`
/// is row i's height).
fn decompose_edge_into_rows(edge: &GraphEdge, row_top_y: &[f32], rows: &mut [RowGeometry]) {
    if edge.child_row >= edge.parent_row {
        return;
    }

    if edge.child_lane == edge.parent_lane {
        let lane = edge.child_lane;
        // Child's row: bottom half (the line emerges from the node).
        if let Some(g) = rows.get_mut(edge.child_row) {
            g.bottom_half_verticals.push((lane, edge.color));
        }
        // Strictly-intermediate rows: full vertical.
        for row in (edge.child_row + 1)..edge.parent_row {
            if let Some(g) = rows.get_mut(row) {
                g.full_verticals.push((lane, edge.color));
            }
        }
        // Parent's row: top half (line ends at the parent's node).
        if let Some(g) = rows.get_mut(edge.parent_row) {
            g.top_half_verticals.push((lane, edge.color));
        }
        return;
    }

    // Cross-lane: full-distance cubic in absolute pixel coords. Node Y
    // is per-row so a pills band on either endpoint pushes that
    // row's anchor down without distorting the rows in between.
    let child_node_y = rows.get(edge.child_row).map_or(NODE_Y, |g| g.node_y);
    let parent_node_y = rows.get(edge.parent_row).map_or(NODE_Y, |g| g.node_y);
    let child_y = row_top_y[edge.child_row] + child_node_y;
    let parent_y = row_top_y[edge.parent_row] + parent_node_y;
    let dy = parent_y - child_y;
    let curve = Cubic {
        p0: (edge.child_lane as f32, child_y),
        p1: (edge.child_lane as f32, child_y + dy * 0.4),
        p2: (edge.parent_lane as f32, parent_y - dy * 0.4),
        p3: (edge.parent_lane as f32, parent_y),
    };

    for row in edge.child_row..=edge.parent_row {
        let row_top = row_top_y[row];
        let row_bot = row_top_y[row + 1];
        let strip_top = if row == edge.child_row { child_y } else { row_top };
        let strip_bot = if row == edge.parent_row { parent_y } else { row_bot };
        if strip_bot - strip_top < 1e-4 {
            continue;
        }
        // y(t) is monotonic for our control-point pattern, so a
        // bisection-based root find always converges to a single root
        // per boundary.
        let t_a = if row == edge.child_row {
            0.0
        } else {
            curve.t_at_y(strip_top)
        };
        let t_b = if row == edge.parent_row {
            1.0
        } else {
            curve.t_at_y(strip_bot)
        };
        let sub = curve.subcurve(t_a, t_b);
        if let Some(g) = rows.get_mut(row) {
            // Translate from absolute pixel y to row-local y (subtract
            // this row's top). x stays in lane units; the render pass
            // converts via `lane_center_x`.
            let to_local = |p: (f32, f32)| -> (f32, f32) { (p.0, p.1 - row_top) };
            g.curves.push(CurveSegment {
                p0: to_local(sub.p0),
                p1: to_local(sub.p1),
                p2: to_local(sub.p2),
                p3: to_local(sub.p3),
                color: edge.color,
            });
        }
    }
}

/// Imperative cubic bezier with a couple of tools for clipping it to
/// horizontal strips. We carry x and y as separate floats because
/// downstream consumers want the path in a different coord system
/// than the math runs in.
#[derive(Clone, Copy, Debug)]
struct Cubic {
    p0: (f32, f32),
    p1: (f32, f32),
    p2: (f32, f32),
    p3: (f32, f32),
}

impl Cubic {
    fn y_at(&self, t: f32) -> f32 {
        let s = 1.0 - t;
        s * s * s * self.p0.1
            + 3.0 * s * s * t * self.p1.1
            + 3.0 * s * t * t * self.p2.1
            + t * t * t * self.p3.1
    }

    /// Bisection root-find for `y_at(t) == target`. Assumes `y_at` is
    /// monotonic increasing, which is the case for the pre-port
    /// control-point pattern (`start_y < start_y + dy*0.4 < end_y -
    /// dy*0.4 < end_y` whenever `dy > 0`).
    fn t_at_y(&self, target: f32) -> f32 {
        if target <= self.p0.1 {
            return 0.0;
        }
        if target >= self.p3.1 {
            return 1.0;
        }
        let mut lo = 0.0_f32;
        let mut hi = 1.0_f32;
        for _ in 0..40 {
            let mid = (lo + hi) * 0.5;
            let y = self.y_at(mid);
            if y < target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo + hi) * 0.5
    }

    fn split(&self, t: f32) -> (Cubic, Cubic) {
        let lerp = |a: (f32, f32), b: (f32, f32)| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        let q01 = lerp(self.p0, self.p1);
        let q12 = lerp(self.p1, self.p2);
        let q23 = lerp(self.p2, self.p3);
        let r012 = lerp(q01, q12);
        let r123 = lerp(q12, q23);
        let s = lerp(r012, r123);
        (
            Cubic {
                p0: self.p0,
                p1: q01,
                p2: r012,
                p3: s,
            },
            Cubic {
                p0: s,
                p1: r123,
                p2: q23,
                p3: self.p3,
            },
        )
    }

    /// Subcurve over `[a, b] ⊆ [0, 1]`. Two splits suffice: split at
    /// `a`, take the right half, then re-parameterize and split that
    /// at the corresponding inner `t` for the original `b`.
    fn subcurve(&self, a: f32, b: f32) -> Cubic {
        if a <= 0.0 && b >= 1.0 {
            return *self;
        }
        let (_, right) = self.split(a.clamp(0.0, 1.0));
        if b >= 1.0 {
            return right;
        }
        let new_t = ((b - a) / (1.0 - a)).clamp(0.0, 1.0);
        let (left, _) = right.split(new_t);
        left
    }
}

/// Compact CI dot for one provider's per-commit rollup. Color reflects
/// the overall state across that provider's checks at this SHA; the
/// tooltip enumerates each check by name + status. A 9 px square with
/// `.radius(4.5)` reads as a circle and matches aetna's existing
/// stroke conventions on small chrome. `key` is required so the dot
/// participates in pointer hit-testing (aetna's tooltip pipeline only
/// fires on keyed nodes).
fn ci_dot(rollup: &ProviderCommitRollup, key: String) -> El {
    let state = rollup.rollup.counts.overall_state();
    let color = match state {
        CiState::Success => tokens::SUCCESS,
        CiState::Failure => tokens::DESTRUCTIVE,
        CiState::Pending => tokens::WARNING,
        CiState::None => tokens::MUTED_FOREGROUND,
    };
    let mut tip = format!("{}: ", rollup.provider.short_label());
    if rollup.rollup.checks.is_empty() {
        tip.push_str("no checks");
    } else {
        let parts: Vec<String> = rollup
            .rollup
            .checks
            .iter()
            .map(|c| {
                let mark = match c.state {
                    CiState::Success => "\u{2713}",  // ✓
                    CiState::Failure => "\u{2717}",  // ✗
                    CiState::Pending => "\u{22ef}",  // ⋯
                    CiState::None => "\u{2014}",     // —
                };
                format!("{mark} {}", c.label)
            })
            .collect();
        tip.push_str(&parts.join(", "));
    }
    El::new(Kind::Group)
        .width(Size::Fixed(9.0))
        .height(Size::Fixed(9.0))
        .fill(color)
        .radius(4.5)
        .key(key)
        .tooltip(tip)
}

/// Stroke width for lane verticals and cross-lane connector curves.
const LINE_WIDTH: f32 = 2.0;
/// Node circle radius. The selected ring sits on top with a thicker
/// stroke for emphasis.
const NODE_RADIUS: f32 = 5.0;
/// Selected-state stroke width for the ring overlay.
const SELECTED_RING_WIDTH: f32 = 2.5;

/// Convert a (possibly out-of-range) lane index into the visible
/// lane's center x. `graph_width` is the row's allocated width; lanes
/// beyond what fits clamp to the rightmost slot — matches the
/// pre-port behavior so deep graphs don't push content off the cell.
fn lane_center_x(lane: usize, graph_width: f32) -> f32 {
    let visible_lanes = ((graph_width / LANE_W).round() as usize).max(1);
    let visual_lane = lane.min(visible_lanes - 1);
    visual_lane as f32 * LANE_W + LANE_W * 0.5
}

/// Build a row's graph cell as a single `vector()` element.
///
/// The asset combines, in z-order:
///   1. lane verticals (`full`, `top_half`, `bottom_half`),
///   2. cross-lane bezier segments,
///   3. the node circle (filled disk, plus a foreground ring on top
///      when this row is selected).
///
/// Strokes use `LineCap::Butt` and the geometry extends fully to the
/// view-box edges — the smoke test (`bin/vector_smoke`) confirmed this
/// tiles cleanly at row boundaries with no MSDF AA seams.
fn graph_cell(
    geom: &RowGeometry,
    node_lane: usize,
    node_color: Color,
    selected: bool,
    graph_width: f32,
) -> El {
    let mut paths: Vec<VectorPath> = Vec::new();
    let h = geom.height;

    let push_vertical = |paths: &mut Vec<VectorPath>, lane: usize, color: Color, y0: f32, y1: f32| {
        let x = lane_center_x(lane, graph_width);
        paths.push(
            PathBuilder::new()
                .move_to(x, y0)
                .line_to(x, y1)
                .stroke_solid(color, LINE_WIDTH)
                .stroke_line_cap(VectorLineCap::Butt)
                .build(),
        );
    };

    let node_y = geom.node_y;
    for &(lane, color) in &geom.full_verticals {
        push_vertical(&mut paths, lane, color, 0.0, h);
    }
    for &(lane, color) in &geom.top_half_verticals {
        push_vertical(&mut paths, lane, color, 0.0, node_y);
    }
    for &(lane, color) in &geom.bottom_half_verticals {
        // Stretches from the node down to the row's bottom — when the
        // row is taller than ROW_HEIGHT (time spacing), this is what
        // visually represents the time gap.
        push_vertical(&mut paths, lane, color, node_y, h);
    }
    for seg in &geom.curves {
        // CurveSegment carries x in lane units; render-time scaling
        // converts to pixels by rounding to the nearest lane and
        // looking up its center via `lane_center_x`. Cubic-bezier
        // control points snap cleanly at integer lane positions for
        // the typical S-curve geometry, but for fractional inputs
        // (after subdivision) we interpolate within the lane width.
        let visible_lanes = ((graph_width / LANE_W).round() as usize).max(1);
        let to_x = |lane_units: f32| {
            let clamped = lane_units.clamp(0.0, (visible_lanes - 1) as f32);
            clamped * LANE_W + LANE_W * 0.5
        };
        paths.push(
            PathBuilder::new()
                .move_to(to_x(seg.p0.0), seg.p0.1)
                .cubic_to(
                    to_x(seg.p1.0),
                    seg.p1.1,
                    to_x(seg.p2.0),
                    seg.p2.1,
                    to_x(seg.p3.0),
                    seg.p3.1,
                )
                .stroke_solid(seg.color, LINE_WIDTH)
                .stroke_line_cap(VectorLineCap::Butt)
                .build(),
        );
    }

    // Node disk — filled circle approximated with cubics. Cubic-bezier
    // approximation of a unit circle uses control-point distance ≈
    // 0.5523 × radius from each endpoint (Stanislaw's constant). Node
    // sits at the row's `node_y` — by default `NODE_Y`, pushed down
    // by `band_height` when this row carries a pills band so the
    // node aligns with the SHA / message line below.
    let cx = lane_center_x(node_lane, graph_width);
    let cy = node_y;
    let r = NODE_RADIUS;
    let k = r * 0.5523;
    paths.push(
        PathBuilder::new()
            .move_to(cx + r, cy)
            .cubic_to(cx + r, cy - k, cx + k, cy - r, cx, cy - r)
            .cubic_to(cx - k, cy - r, cx - r, cy - k, cx - r, cy)
            .cubic_to(cx - r, cy + k, cx - k, cy + r, cx, cy + r)
            .cubic_to(cx + k, cy + r, cx + r, cy + k, cx + r, cy)
            .fill_solid(node_color)
            .build(),
    );
    if selected {
        // Foreground ring sits on top of the disk, painted as a stroke
        // around the circle with the selection color.
        paths.push(
            PathBuilder::new()
                .move_to(cx + r, cy)
                .cubic_to(cx + r, cy - k, cx + k, cy - r, cx, cy - r)
                .cubic_to(cx - k, cy - r, cx - r, cy - k, cx - r, cy)
                .cubic_to(cx - r, cy + k, cx - k, cy + r, cx, cy + r)
                .cubic_to(cx + k, cy + r, cx + r, cy + k, cx + r, cy)
                .stroke_solid(tokens::FOREGROUND, SELECTED_RING_WIDTH)
                .stroke_line_cap(VectorLineCap::Butt)
                .build(),
        );
    }

    let asset = VectorAsset::from_paths([0.0, 0.0, graph_width, h], paths);
    vector(asset)
        .width(Size::Fixed(graph_width))
        .height(Size::Fixed(h))
}

/// Static, themable pill chrome — small caption pinned to a tinted
/// background with a half-alpha border. Used for branch / tag / HEAD /
/// orphan / clean-worktree rendering inside commit rows.
///
/// `key` is required so the pill takes part in pointer hit-testing
/// (aetna only fires tooltips and dispatches clicks on keyed nodes).
/// Pills with a `worktree:` key prefix opt into the focus chain so
/// keyboard navigation can land on them; other keys (informational
/// branch / tag / HEAD / ORPHAN / PINNED chrome) stay out of focus
/// to keep tab-traversal short.
fn pill(
    label: impl Into<String>,
    fg: Color,
    bg_alpha: u8,
    key: String,
    tooltip: Option<String>,
) -> El {
    let focusable = key.starts_with("worktree:");
    let mut row_el = row([text(label.into()).caption().text_color(fg)])
        .padding(Sides::xy(tokens::SPACE_2, 2.0))
        .gap(tokens::SPACE_1)
        .align(Align::Center)
        .fill(fg.with_alpha(bg_alpha))
        .stroke(fg.with_alpha(120))
        .key(key);
    if focusable {
        row_el = row_el.focusable();
    }
    if let Some(tip) = tooltip {
        row_el = row_el.tooltip(tip);
    }
    row_el
}

/// Decoration set for a single commit row — branch tips at this
/// commit, tags at this commit, clean worktrees whose HEAD is this
/// commit. Pre-computed in `history_view` so the per-row closure
/// stays cheap.
#[derive(Clone, Default)]
struct RowPills {
    /// (branch name, kind) pairs.
    branches: Vec<(String, BranchKind)>,
    tags: Vec<String>,
    /// Names of clean worktrees pointing here. Dirty worktrees show
    /// their pill on the synthetic row instead, so this list excludes
    /// them.
    clean_worktrees: Vec<String>,
}

#[derive(Clone, Copy)]
enum BranchKind {
    /// Active worktree's branch — green.
    Head,
    /// Local non-head — slate-blue / primary.
    Local,
    /// Remote-tracking branch — info / cyan.
    Remote,
}

impl BranchKind {
    fn color(self) -> Color {
        match self {
            Self::Head => tokens::SUCCESS,
            Self::Local => tokens::PRIMARY,
            Self::Remote => tokens::INFO,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_row(
    commit: &CommitInfo,
    layout: Option<&CommitLayout>,
    geom: &RowGeometry,
    graph_width: f32,
    pills: &RowPills,
    ci_rollups: Option<&[ProviderCommitRollup]>,
    is_detached_head_here: bool,
    is_pinned: bool,
    idx: usize,
    selected: bool,
    avatar: Option<Image>,
) -> El {
    if commit.is_synthetic {
        return synthetic_row(commit, layout, geom, graph_width, idx, selected);
    }

    let (lane, color) = match layout {
        Some(l) => (l.lane, l.color),
        None => (0, ORPHAN_COLOR),
    };
    let when = commit.relative_time();
    let summary = if commit.summary.is_empty() {
        "(no summary)".to_string()
    } else {
        commit.summary.clone()
    };

    // Pills band — PINNED first so the parent-repo annotation reads
    // adjacent to the SHA on the line below, then clean worktrees,
    // branches, tags, detached HEAD, and orphan. "Any pill at all"
    // promotes the row to the two-row layout regardless of which kind
    // was responsible.
    let mut pill_kids: Vec<El> = Vec::new();
    if is_pinned {
        pill_kids.push(pill(
            "PINNED",
            tokens::INFO,
            44,
            format!("commit:{idx}.pinned"),
            Some("Parent repo pins this commit".to_string()),
        ));
    }
    for wt_name in &pills.clean_worktrees {
        pill_kids.push(pill(
            format!("WT: {wt_name}"),
            tokens::WARNING,
            40,
            format!("worktree:{wt_name}"),
            Some("Switch to this worktree".to_string()),
        ));
    }
    for (name, kind) in &pills.branches {
        pill_kids.push(pill(
            name.clone(),
            kind.color(),
            44,
            format!("commit:{idx}.branch:{name}"),
            Some(name.clone()),
        ));
    }
    for tag in &pills.tags {
        pill_kids.push(pill(
            format!("\u{25C6} {tag}"),
            tokens::WARNING,
            40,
            format!("commit:{idx}.tag:{tag}"),
            Some(tag.clone()),
        ));
    }
    if is_detached_head_here && pills.branches.is_empty() {
        pill_kids.push(pill(
            "HEAD",
            tokens::SUCCESS,
            44,
            format!("commit:{idx}.head"),
            Some("Detached HEAD".to_string()),
        ));
    }
    if commit.is_orphaned {
        pill_kids.push(pill(
            "ORPHAN",
            ORPHAN_COLOR,
            44,
            format!("commit:{idx}.orphan"),
            commit.orphan_source.clone(),
        ));
    }

    // Subject + optional body excerpt. `Fill(1.0)` lets the message
    // absorb whatever space is left after the left-anchored chrome
    // (SHA + avatar) and the right-aligned metadata cluster (diff
    // stats, CI dots, time) claim their intrinsic widths. The summary
    // tooltip carries the unellipsized subject (and full body when
    // present) so users get the whole message on hover even when the
    // line is truncated.
    let summary_tooltip = match commit.body_full.as_deref().filter(|s| !s.is_empty()) {
        Some(body) => format!("{summary}\n\n{body}"),
        None => summary.clone(),
    };
    let mut summary_children: Vec<El> = vec![text(summary).ellipsis()];
    if let Some(body) = commit.body_excerpt.as_deref().filter(|s| !s.is_empty()) {
        summary_children.push(
            text(format!("\u{2014} {body}"))
                .muted()
                .ellipsis()
                .width(Size::Fill(1.0)),
        );
    }
    let summary_row = row(summary_children)
        .gap(tokens::SPACE_2)
        .align(Align::Center)
        .width(Size::Fill(1.0))
        .clip()
        .key(format!("commit:{idx}.summary"))
        .tooltip(summary_tooltip);

    // Main content row: SHA, avatar, message-fill, then a fixed
    // right-side cluster of [+N -M, CI dots, time]. Avatar is
    // tooltip-only for the author name — dropping the inline
    // "Author · time" text keeps the right cluster predictable in
    // width across rows. Tooltips on each leaf carry the long-form
    // info that doesn't fit in the cell. Each leaf carries a
    // `commit:{idx}.<part>` key so it's a hit-test target (aetna's
    // tooltip pipeline only fires on keyed nodes); the click handler
    // strips the trailing `.<part>` and routes the click to commit
    // selection on the row's idx.
    let mut main_children: Vec<El> = vec![
        text(commit.short_id.clone())
            .mono()
            .muted()
            .key(format!("commit:{idx}.sha"))
            .tooltip(commit.id.to_string()),
        author_avatar(&commit.author, avatar, format!("commit:{idx}.avatar")),
        summary_row,
    ];
    if let Some(chip) = diff_stats_chip(commit, format!("commit:{idx}.diff")) {
        main_children.push(chip);
    }
    if let Some(rollups) = ci_rollups {
        let ci_kids: Vec<El> = rollups
            .iter()
            .enumerate()
            .map(|(i, r)| ci_dot(r, format!("commit:{idx}.ci{i}")))
            .collect();
        if !ci_kids.is_empty() {
            main_children.push(
                row(ci_kids)
                    .gap(tokens::SPACE_1)
                    .align(Align::Center),
            );
        }
    }
    main_children.push(
        text(when)
            .muted()
            .key(format!("commit:{idx}.time"))
            .tooltip(commit.absolute_time()),
    );

    let main_row = row(main_children)
        .gap(tokens::SPACE_3)
        .align(Align::Center)
        .width(Size::Fill(1.0))
        .height(Size::Fixed(ROW_HEIGHT));

    // Stack the pills band above the main row when one is present;
    // otherwise the main row sits directly in the outer flex.
    let content: El = if pill_kids.is_empty() {
        main_row
    } else {
        let band = row(pill_kids)
            .gap(tokens::SPACE_2)
            .align(Align::End)
            .height(Size::Fixed(PILLS_BAND_HEIGHT))
            .width(Size::Fill(1.0));
        column([band, main_row])
            .gap(0.0)
            .width(Size::Fill(1.0))
    };

    // Outer row: graph cell (full geom.height) + content column.
    // `Align::Start` anchors the content at the top of the row strip
    // so the pills band sits flush above the main row, and any extra
    // height from time spacing hangs below as breathing room — also
    // brings the main row's vertical center back in line with the
    // graph node at `geom.node_y` for both single- and two-row cases.
    let inner = row([
        graph_cell(geom, lane, color, selected, graph_width),
        content,
    ])
    .key(format!("commit:{idx}"))
    .focusable()
    .gap(tokens::SPACE_3)
    .padding(Sides::xy(tokens::SPACE_2, 0.0))
    .height(Size::Fixed(geom.height))
    .width(Size::Fill(1.0))
    .align(Align::Start)
    .clip();

    let inner = if selected { inner.selected() } else { inner };

    // Outer wrapper reserves a right gutter for the virtual_list
    // scrollbar thumb so the focusable inner doesn't overlap it. The
    // zebra tint lives here (not on the inner) so the stripe still
    // spans full width, which keeps the rhythm visually consistent
    // across the scrollbar gutter.
    let mut outer = column([inner])
        .width(Size::Fill(1.0))
        .padding(Sides {
            left: 0.0,
            right: SCROLLBAR_GUTTER,
            top: 0.0,
            bottom: 0.0,
        });
    if !selected && idx % 2 == 1 {
        // Zebra striping on every other row — a faint MUTED tint that
        // helps the eye track across long lines without competing with
        // the selected-row highlight (which paints its own fill via
        // `.selected()`).
        outer = outer.fill(tokens::MUTED.with_alpha(40));
    }
    outer
}

/// Compact `+N -M` chip rendered to the right of the commit summary,
/// before the author/time column. Returns `None` when both counts
/// are zero — async diff-stats fetching may not have caught up yet,
/// or the commit may genuinely be empty (merge with no conflicts).
/// The caller skips the column entirely in that case so the row's
/// gap rhythm doesn't reserve a phantom slot.
fn diff_stats_chip(commit: &CommitInfo, key: String) -> Option<El> {
    if commit.insertions == 0 && commit.deletions == 0 {
        return None;
    }
    let tip = format!(
        "{} insertion{}, {} deletion{}",
        commit.insertions,
        if commit.insertions == 1 { "" } else { "s" },
        commit.deletions,
        if commit.deletions == 1 { "" } else { "s" },
    );
    Some(
        row([
            text(format!("+{}", commit.insertions))
                .caption()
                .mono()
                .text_color(tokens::SUCCESS),
            text(format!("-{}", commit.deletions))
                .caption()
                .mono()
                .text_color(tokens::DESTRUCTIVE),
        ])
        .gap(tokens::SPACE_1)
        .align(Align::Center)
        .key(key)
        .tooltip(tip),
    )
}

/// Render a synthetic "uncommitted changes" row.
///
/// Visually distinct from real commits — amber lane node, no SHA, an
/// inline `WT:{name}` pill that re-routes to the worktree-switch
/// handler so users can jump straight to the right staging area from
/// the History view. Selection is suppressed (see
/// `RepoTab::select_commit`), so the row click is a no-op except via
/// the WT pill.
fn synthetic_row(
    commit: &CommitInfo,
    layout: Option<&CommitLayout>,
    geom: &RowGeometry,
    graph_width: f32,
    idx: usize,
    selected: bool,
) -> El {
    let amber = tokens::WARNING;
    let lane = layout.map(|l| l.lane).unwrap_or(0);

    let mut children: Vec<El> = vec![graph_cell(geom, lane, amber, selected, graph_width)];
    if let Some(name) = commit.synthetic_wt_name.as_deref() {
        children.push(pill(
            format!("WT: {name}"),
            amber,
            40,
            format!("worktree:{name}"),
            Some("Switch to this worktree".to_string()),
        ));
    }
    children.push(
        text(if commit.summary.is_empty() {
            "Uncommitted changes".to_string()
        } else {
            commit.summary.clone()
        })
        .text_color(amber)
        .ellipsis()
        .width(Size::Fill(1.0)),
    );
    children.push(text(commit.relative_time()).muted().caption());

    let inner = row(children)
        .key(format!("commit:{idx}"))
        .focusable()
        .gap(tokens::SPACE_3)
        .padding(Sides::xy(tokens::SPACE_2, 0.0))
        .height(Size::Fixed(geom.height))
        .width(Size::Fill(1.0))
        .align(Align::Center)
        .clip();
    // See `build_row` — wrap so the focusable rect doesn't overlap the
    // virtual_list scrollbar thumb on the right edge.
    column([inner])
        .width(Size::Fill(1.0))
        .padding(Sides {
            left: 0.0,
            right: SCROLLBAR_GUTTER,
            top: 0.0,
            bottom: 0.0,
        })
}

/// Estimated row height for `virtual_list_dyn` — the minimum a row
/// can be. Time-spaced rows grow taller; the runtime measures actuals
/// on first viewport entry and caches them, so this is just the
/// initial scrollbar-thumb sizing.
const EST_ROW_HEIGHT: f32 = ROW_HEIGHT;

/// Routed key for the history-pane search input. Events for this
/// key get folded into `tab.search_query` via `text_input::apply_event`.
pub const SEARCH_INPUT_KEY: &str = "history:search";

/// History pane composer. Returns the center-pane `El` for the
/// History view mode. Wraps the virtualized commit list in a column
/// with a search input + count chip, then the rows themselves. Rows
/// whose subject / author / short-id don't match the active query
/// dim to ~30% opacity so the matching set stands out without
/// disrupting the graph's visual integrity (filtering would skip
/// rows and break the lane verticals between adjacent commits).
pub fn history_view(
    tab: &RepoTab,
    selection: &Selection,
    avatars: HashMap<String, Image>,
) -> El {
    if tab.commits.is_empty() {
        return column([
            text("No commits").muted(),
            text("This repo has no reachable commits — make one and refresh.").muted(),
        ])
        .gap(tokens::SPACE_2)
        .padding(tokens::SPACE_4)
        .width(Size::Fill(1.0))
        .height(Size::Fill(1.0));
    }

    // Pre-compute which rows match the active search query. When the
    // query is empty, every row matches (no dimming).
    let search_query = tab.search_query.clone();
    let match_flags: Vec<bool> = if search_query.is_empty() {
        vec![true; tab.commits.len()]
    } else {
        let q = search_query.to_lowercase();
        tab.commits
            .iter()
            .map(|c| commit_matches_query(c, &q))
            .collect()
    };
    let match_count: usize = match_flags.iter().filter(|m| **m).count();

    let header_text = if !search_query.is_empty() {
        format!("{match_count} match{} · {} commits",
            if match_count == 1 { "" } else { "es" },
            tab.commits.len())
    } else {
        match tab.selected_commit {
            Some(oid) => match tab.commits.iter().find(|c| c.id == oid) {
                Some(c) => format!(
                    "{} · {} · {}",
                    &c.short_id,
                    c.author,
                    if c.summary.is_empty() {
                        "(no summary)"
                    } else {
                        &c.summary
                    }
                ),
                None => format!("{} commits", tab.commits.len()),
            },
            None => format!("{} commits", tab.commits.len()),
        }
    };

    // virtual_list takes a Fn(usize) -> El, so we clone the data the
    // closure needs — Vec<CommitInfo> + Vec<CommitLayout> + parallel
    // pill data. The layout / pill lookups go through flat parallel
    // Vecs instead of HashMaps so the closure stays Send + Sync.
    let layouts: Vec<Option<CommitLayout>> = tab
        .commits
        .iter()
        .map(|c| tab.graph_layout.get(&c.id).cloned())
        .collect();
    let graph_width = tab.graph_layout.graph_width;
    let pills_per_row = build_row_pills(tab);
    let active_head_oid = tab.active_view().and_then(|v| v.head_oid);
    let detached_flags: Vec<bool> = tab
        .commits
        .iter()
        .map(|c| {
            // Surface the detached-HEAD pill only when no branch tip
            // points here — otherwise the branch pill already conveys
            // "this is HEAD" via its green tint.
            active_head_oid == Some(c.id)
                && !tab.branch_tips.iter().any(|t| t.oid == c.id && !t.is_remote)
        })
        .collect();
    let ci_per_row: Vec<Option<Vec<ProviderCommitRollup>>> = tab
        .commits
        .iter()
        .map(|c| tab.ci_per_commit.get(&c.id.to_string()).cloned())
        .collect();
    let pinned_flags: Vec<bool> = tab
        .commits
        .iter()
        .map(|c| tab.pinned_oid == Some(c.id))
        .collect();

    // Derive per-row band heights from the inputs that drive the
    // pills band: any branch / tag / clean-worktree / detached-HEAD
    // / orphan / PINNED pill triggers a band above the main content.
    // Synthetic ("uncommitted changes") rows never carry a band —
    // their WT pill stays inline.
    let band_heights: Vec<f32> = tab
        .commits
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if c.is_synthetic {
                return 0.0;
            }
            let p = &pills_per_row[i];
            let any_pill = !p.clean_worktrees.is_empty()
                || !p.branches.is_empty()
                || !p.tags.is_empty()
                || (detached_flags[i] && p.branches.is_empty())
                || c.is_orphaned
                || pinned_flags[i];
            if any_pill { PILLS_BAND_HEIGHT } else { 0.0 }
        })
        .collect();
    let geom_per_row: Vec<RowGeometry> = tab
        .graph_layout
        .row_geometry_with_bands(&tab.commits, &band_heights);
    let commits = tab.commits.clone();
    let selected_oid = tab.selected_commit;

    // Search bar is hidden by default; Ctrl+F flips `history_search_open`
    // to true and the row appears beneath the count chip. Escape closes
    // it and clears the query (handled in `WhisperApp::on_event`).
    let mut header_children: Vec<El> = vec![
        row([text(header_text).caption().muted()]).align(Align::Center),
    ];
    if tab.history_search_open {
        let search_input = text_input(&tab.search_query, selection, SEARCH_INPUT_KEY)
            .width(Size::Fill(1.0));
        header_children.push(
            row([
                icon(IconName::Search).icon_size(tokens::ICON_SM).muted(),
                search_input,
            ])
            .gap(tokens::SPACE_2)
            .align(Align::Center),
        );
    }

    card([
        card_header(header_children)
        .padding(Sides::xy(tokens::SPACE_3, tokens::SPACE_2))
        .gap(tokens::SPACE_2)
        .fill(tokens::MUTED),
        card_content([virtual_list_dyn(commits.len(), EST_ROW_HEIGHT, move |i| {
            let c = &commits[i];
            let selected = selected_oid == Some(c.id);
            // Empty fallback geom keeps the row paintable when the
            // layout pass hasn't caught up to the commit list yet
            // (e.g., right after a refresh — graph_layout.build runs
            // synchronously, so this normally never triggers).
            let empty = RowGeometry::default();
            let geom = geom_per_row.get(i).unwrap_or(&empty);
            let matches = match_flags.get(i).copied().unwrap_or(true);
            let avatar = avatars.get(&c.author_email).cloned();
            let row_el = build_row(
                c,
                layouts[i].as_ref(),
                geom,
                graph_width,
                &pills_per_row[i],
                ci_per_row[i].as_deref(),
                detached_flags[i],
                pinned_flags[i],
                i,
                selected,
                avatar,
            );
            if matches { row_el } else { row_el.opacity(0.3) }
        })
        .key("commits")
        .height(Size::Fill(1.0))])
        .padding(0.0)
        .height(Size::Fill(1.0)),
    ])
    .width(Size::Fill(1.0))
    .height(Size::Fill(1.0))
}

/// Lower-case substring match across the fields the pre-port
/// searched: subject, author, short SHA. The full SHA's prefix is
/// also checked so paste-a-SHA navigation still works.
fn commit_matches_query(c: &CommitInfo, lower_query: &str) -> bool {
    if c.summary.to_lowercase().contains(lower_query) {
        return true;
    }
    if c.author.to_lowercase().contains(lower_query) {
        return true;
    }
    if c.short_id.to_lowercase().contains(lower_query) {
        return true;
    }
    if c.id.to_string().to_lowercase().starts_with(lower_query) {
        return true;
    }
    false
}

/// Pre-compute the `RowPills` for each commit: walk branch tips,
/// tags, and clean worktree views, indexing them by their oid, then
/// gather them onto each commit row in `tab.commits` order. Faster
/// than per-row filtering on big histories.
fn build_row_pills(tab: &RepoTab) -> Vec<RowPills> {
    let mut by_oid_branches: HashMap<Oid, Vec<(String, BranchKind)>> = HashMap::new();
    for tip in &tab.branch_tips {
        // Drop `origin/HEAD`-style symref aliases — git2 enumerates them
        // as branches, but they're just pointers to a real branch and
        // would double up the pill set.
        if tip.is_remote && matches!(tip.name.split_once('/'), Some((_, "HEAD"))) {
            continue;
        }
        let kind = if tip.is_remote {
            BranchKind::Remote
        } else if tip.is_head {
            BranchKind::Head
        } else {
            BranchKind::Local
        };
        by_oid_branches
            .entry(tip.oid)
            .or_default()
            .push((tip.name.clone(), kind));
    }

    let mut by_oid_tags: HashMap<Oid, Vec<String>> = HashMap::new();
    for tag in &tab.tags {
        by_oid_tags
            .entry(tag.oid)
            .or_default()
            .push(tag.name.clone());
    }

    // Clean worktrees: those whose status reports zero dirty files. A
    // dirty worktree's pill belongs on the synthetic row above its
    // HEAD, not on the HEAD itself, so we exclude them here.
    let mut by_oid_clean_wts: HashMap<Oid, Vec<String>> = HashMap::new();
    for view in tab.worktree_views.values() {
        if view.status.total_files() != 0 {
            continue;
        }
        if let Some(head) = view.head_oid {
            by_oid_clean_wts
                .entry(head)
                .or_default()
                .push(view.name.clone());
        }
    }

    tab.commits
        .iter()
        .map(|c| RowPills {
            branches: by_oid_branches.get(&c.id).cloned().unwrap_or_default(),
            tags: by_oid_tags.get(&c.id).cloned().unwrap_or_default(),
            clean_worktrees: by_oid_clean_wts.get(&c.id).cloned().unwrap_or_default(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_color() -> Color {
        tokens::PRIMARY
    }

    #[test]
    fn cubic_t_at_y_recovers_endpoints() {
        let c = Cubic {
            p0: (0.0, 0.0),
            p1: (0.0, 0.4),
            p2: (3.0, 0.6),
            p3: (3.0, 1.0),
        };
        assert!((c.t_at_y(0.0) - 0.0).abs() < 1e-3);
        assert!((c.t_at_y(1.0) - 1.0).abs() < 1e-3);
        // y(0.5) should be 0.5 by symmetry of the control-point pattern.
        assert!((c.y_at(c.t_at_y(0.5)) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn cubic_subcurve_endpoints_match_y_at() {
        let c = Cubic {
            p0: (0.0, 0.0),
            p1: (0.0, 0.4),
            p2: (3.0, 0.6),
            p3: (3.0, 1.0),
        };
        let sub = c.subcurve(0.25, 0.75);
        // Sub-curve start should equal full-curve y(0.25); end should
        // equal full-curve y(0.75) — within bisection tolerance.
        let target_a = c.y_at(0.25);
        let target_b = c.y_at(0.75);
        assert!((sub.p0.1 - target_a).abs() < 1e-3);
        assert!((sub.p3.1 - target_b).abs() < 1e-3);
    }

    /// Build `row_top_y` for `n` uniform-height rows. The decompose
    /// function takes one entry per row plus a sentinel for the total
    /// content height.
    fn uniform_offsets(n: usize) -> Vec<f32> {
        (0..=n).map(|i| i as f32 * ROW_HEIGHT).collect()
    }

    #[test]
    fn decompose_same_lane_emits_top_full_bottom_verticals() {
        let edge = GraphEdge {
            child_row: 0,
            child_lane: 1,
            parent_row: 3,
            parent_lane: 1,
            color: dummy_color(),
        };
        let mut rows = vec![RowGeometry::default(); 4];
        decompose_edge_into_rows(&edge, &uniform_offsets(4), &mut rows);
        // child row gets bottom-half (line emerges below the node).
        assert_eq!(rows[0].bottom_half_verticals.len(), 1);
        assert_eq!(rows[0].full_verticals.len(), 0);
        assert_eq!(rows[0].top_half_verticals.len(), 0);
        // intermediate rows get full verticals.
        assert_eq!(rows[1].full_verticals.len(), 1);
        assert_eq!(rows[2].full_verticals.len(), 1);
        // parent row gets top-half (line ends at the node).
        assert_eq!(rows[3].top_half_verticals.len(), 1);
    }

    #[test]
    fn decompose_cross_lane_emits_one_curve_per_spanned_row() {
        let edge = GraphEdge {
            child_row: 0,
            child_lane: 0,
            parent_row: 3,
            parent_lane: 2,
            color: dummy_color(),
        };
        let mut rows = vec![RowGeometry::default(); 4];
        decompose_edge_into_rows(&edge, &uniform_offsets(4), &mut rows);
        // 4 spanned rows = 4 curve segments; no verticals on cross-lane.
        assert_eq!(rows[0].curves.len(), 1);
        assert_eq!(rows[1].curves.len(), 1);
        assert_eq!(rows[2].curves.len(), 1);
        assert_eq!(rows[3].curves.len(), 1);
        for r in &rows {
            assert!(r.full_verticals.is_empty());
            assert!(r.top_half_verticals.is_empty());
            assert!(r.bottom_half_verticals.is_empty());
        }
    }

    #[test]
    fn decompose_cross_lane_segment_y_spans_row_strip() {
        // Each row's curve segment should start at row-local y matching
        // the strip's top and end at the strip's bottom — modulo the
        // child row (starts at NODE_Y) and parent row (ends at NODE_Y).
        let edge = GraphEdge {
            child_row: 0,
            child_lane: 0,
            parent_row: 2,
            parent_lane: 1,
            color: dummy_color(),
        };
        let mut rows = vec![RowGeometry::default(); 3];
        decompose_edge_into_rows(&edge, &uniform_offsets(3), &mut rows);

        let row0_curve = &rows[0].curves[0];
        assert!((row0_curve.p0.1 - NODE_Y).abs() < 0.5);
        assert!((row0_curve.p3.1 - ROW_HEIGHT).abs() < 0.5);

        let row1_curve = &rows[1].curves[0];
        assert!((row1_curve.p0.1 - 0.0).abs() < 0.5);
        assert!((row1_curve.p3.1 - ROW_HEIGHT).abs() < 0.5);

        let row2_curve = &rows[2].curves[0];
        assert!((row2_curve.p0.1 - 0.0).abs() < 0.5);
        assert!((row2_curve.p3.1 - NODE_Y).abs() < 0.5);
    }

    #[test]
    fn compute_row_heights_clamps_to_min_for_dense_commits() {
        // Two commits 60 seconds apart — well below TIME_BASE_SECONDS,
        // so the height should be the minimum (ROW_HEIGHT).
        let commits = vec![
            CommitInfo {
                time: 1_000_000,
                ..test_commit()
            },
            CommitInfo {
                time: 1_000_000 - 60,
                ..test_commit()
            },
        ];
        let h = compute_row_heights(&commits);
        assert_eq!(h.len(), 2);
        assert!((h[0] - ROW_HEIGHT).abs() < 1.0);
        // Last row always uses minimum.
        assert!((h[1] - ROW_HEIGHT).abs() < 1.0);
    }

    #[test]
    fn compute_row_heights_saturates_at_max_for_long_gaps() {
        // 60-day gap — past TIME_MAX_DELTA_SECONDS, so the row should
        // saturate at ROW_HEIGHT + MAX_EXTRA_HEIGHT.
        let commits = vec![
            CommitInfo {
                time: 1_000_000_000,
                ..test_commit()
            },
            CommitInfo {
                time: 1_000_000_000 - 60 * 24 * 3600,
                ..test_commit()
            },
        ];
        let h = compute_row_heights(&commits);
        let expected = (ROW_HEIGHT + MAX_EXTRA_HEIGHT).round();
        assert!((h[0] - expected).abs() < 1.0);
    }

    fn test_commit() -> CommitInfo {
        CommitInfo {
            id: Oid::zero(),
            short_id: String::new(),
            summary: String::new(),
            body_excerpt: None,
            body_full: None,
            author: String::new(),
            author_email: String::new(),
            time: 0,
            parent_ids: Vec::new(),
            insertions: 0,
            deletions: 0,
            is_synthetic: false,
            synthetic_wt_name: None,
            is_orphaned: false,
            orphan_source: None,
        }
    }
}

