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
//! `history_view` composes the `virtual_list` and per-row `El`. Rows
//! are fixed-height; variable-height (timeline gaps, inline expand)
//! is deferred until aetna ships variable-height virtualization.

use std::collections::HashMap;

use aetna_core::{Color, El, prelude::*};
use git2::Oid;

use crate::git::CommitInfo;
use crate::repo_tab::RepoTab;

pub const ROW_HEIGHT: f32 = 28.0;
pub const GRAPH_WIDTH: f32 = 140.0;
pub const LANE_COUNT_VISUAL: u8 = 6;

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

#[derive(Clone, Debug)]
pub struct CommitLayout {
    pub lane: usize,
    pub color: Color,
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
}

impl GraphLayout {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn build(&mut self, commits: &[CommitInfo]) {
        self.layouts.clear();
        self.active_lanes.clear();
        self.max_lane = 0;

        let commit_set: HashMap<Oid, ()> = commits.iter().map(|c| (c.id, ())).collect();

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
    }

    pub fn get(&self, id: &Oid) -> Option<&CommitLayout> {
        self.layouts.get(id)
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

/// One row's graph cell — paints a vertical lane line plus a circle
/// node via the `commit_node` shader (registered in `ui_app.rs`).
fn graph_cell(lane: usize, color: Color, selected: bool) -> El {
    let bg = tokens::BACKGROUND;
    let ring_color = if selected { tokens::FOREGROUND } else { color };
    let ring_w = if selected { 2.5 } else { 1.5 };
    let radius = 5.0;
    let line_w = 2.0;
    // Clamp visually so very deep graphs don't push the node off the
    // cell — beyond LANE_COUNT_VISUAL we stack onto the rightmost lane.
    let visual_lane = lane.min(LANE_COUNT_VISUAL as usize - 1);
    let lane_frac = (visual_lane as f32 + 0.5) / LANE_COUNT_VISUAL as f32;

    El::new(Kind::Custom("graph_cell"))
        .width(Size::Fixed(GRAPH_WIDTH))
        .height(Size::Fixed(ROW_HEIGHT))
        .shader(
            ShaderBinding::custom("commit_node")
                .color("vec_a", bg)
                .color("vec_b", ring_color)
                .vec4("vec_c", [radius, ring_w, line_w, lane_frac]),
        )
        // Tag the cell with lane color so the bundle still reflects
        // lane palette even though the shader paints the visual.
        .fill(color)
}

fn build_row(commit: &CommitInfo, layout: Option<&CommitLayout>, idx: usize, selected: bool) -> El {
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

    let row_el = row([
        graph_cell(lane, color, selected),
        text(commit.short_id.clone()).mono().muted(),
        text(summary),
        spacer(),
        text(format!("{} · {}", commit.author, when)).muted(),
    ])
    .key(format!("commit:{idx}"))
    .focusable()
    .gap(tokens::SPACE_MD)
    .padding(Sides::xy(tokens::SPACE_SM, 0.0))
    .height(Size::Fixed(ROW_HEIGHT))
    .align(Align::Center);

    if selected { row_el.selected() } else { row_el }
}

/// History pane composer. Returns the center-pane `El` for the
/// History view mode. Wraps the virtualized commit list in a column
/// with a header line summarizing count + selection.
pub fn history_view(tab: &RepoTab) -> El {
    if tab.commits.is_empty() {
        return column([
            text("No commits").muted(),
            text("This repo has no reachable commits — make one and refresh.").muted(),
        ])
        .gap(tokens::SPACE_SM)
        .padding(tokens::SPACE_LG)
        .width(Size::Fill(1.0))
        .height(Size::Fill(1.0));
    }

    let header_text = match tab.selected_commit {
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
    };

    // virtual_list takes a Fn(usize) -> El, so we clone the data the
    // closure needs — Vec<CommitInfo> + Vec<CommitLayout>. The layout
    // lookup goes through a flat parallel Vec instead of the HashMap
    // so the closure stays Send + Sync.
    let layouts: Vec<Option<CommitLayout>> = tab
        .commits
        .iter()
        .map(|c| tab.graph_layout.get(&c.id).cloned())
        .collect();
    let commits = tab.commits.clone();
    let selected_oid = tab.selected_commit;

    let header = row([text(header_text).caption().muted()])
        .padding(Sides::xy(tokens::SPACE_MD, tokens::SPACE_SM))
        .fill(tokens::MUTED)
        .stroke(tokens::BORDER);

    column([
        header,
        virtual_list(commits.len(), ROW_HEIGHT, move |i| {
            let c = &commits[i];
            let selected = selected_oid == Some(c.id);
            build_row(c, layouts[i].as_ref(), i, selected)
        })
        .key("commits")
        .height(Size::Fill(1.0)),
    ])
    .gap(0.0)
    .fill(tokens::CARD)
    .stroke(tokens::BORDER)
    .width(Size::Fill(1.0))
    .height(Size::Fill(1.0))
}
