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

use crate::ci::{CiState, ProviderCommitRollup};
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

/// Compact CI dot for one provider's per-commit rollup. Color reflects
/// the overall state across that provider's checks at this SHA; the
/// tooltip enumerates each check by name + status. A 9 px square with
/// `.radius(4.5)` reads as a circle and matches aetna's existing
/// stroke conventions on small chrome.
fn ci_dot(rollup: &ProviderCommitRollup) -> El {
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
        .tooltip(tip)
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

/// Static, themable pill chrome — small caption pinned to a tinted
/// background with a half-alpha border. Used for branch / tag / HEAD /
/// orphan / clean-worktree rendering inside commit rows.
///
/// `route_key` makes the pill clickable (worktree switch); when None
/// the pill is purely informational. The `tooltip` hangs verbose
/// metadata (reflog source for orphans, full branch names) without
/// crowding the row.
fn pill(
    label: impl Into<String>,
    fg: Color,
    bg_alpha: u8,
    route_key: Option<String>,
    tooltip: Option<String>,
) -> El {
    let mut row_el = row([text(label.into()).caption().text_color(fg)])
        .padding(Sides::xy(tokens::SPACE_2, tokens::SPACE_1))
        .gap(tokens::SPACE_1)
        .align(Align::Center)
        .fill(fg.with_alpha(bg_alpha))
        .stroke(fg.with_alpha(120));
    if let Some(key) = route_key {
        row_el = row_el.key(key).focusable();
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

fn build_row(
    commit: &CommitInfo,
    layout: Option<&CommitLayout>,
    pills: &RowPills,
    ci_rollups: Option<&[ProviderCommitRollup]>,
    is_detached_head_here: bool,
    idx: usize,
    selected: bool,
) -> El {
    if commit.is_synthetic {
        return synthetic_row(commit, layout, idx, selected);
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

    let mut children: Vec<El> = vec![
        graph_cell(lane, color, selected),
        text(commit.short_id.clone()).mono().muted(),
    ];
    if let Some(rollups) = ci_rollups {
        for r in rollups {
            children.push(ci_dot(r));
        }
    }

    // Pill priority order matches the old graph: clean worktrees
    // first (most likely to be cut by overflow), then branches, then
    // tags, then a HEAD pill for detached-HEAD rows, then orphan
    // marker. Aetna handles row overflow itself — there's no manual
    // "+N" overflow badge for now.
    for wt_name in &pills.clean_worktrees {
        children.push(pill(
            format!("WT: {wt_name}"),
            tokens::WARNING,
            40,
            Some(format!("worktree:{wt_name}")),
            Some("Switch to this worktree".to_string()),
        ));
    }
    for (name, kind) in &pills.branches {
        children.push(pill(
            name.clone(),
            kind.color(),
            44,
            None,
            Some(name.clone()),
        ));
    }
    for tag in &pills.tags {
        children.push(pill(
            format!("\u{25C6} {tag}"),
            tokens::WARNING,
            40,
            None,
            Some(tag.clone()),
        ));
    }
    if is_detached_head_here && pills.branches.is_empty() {
        children.push(pill(
            "HEAD",
            tokens::SUCCESS,
            44,
            None,
            Some("Detached HEAD".to_string()),
        ));
    }
    if commit.is_orphaned {
        children.push(pill(
            "ORPHAN",
            ORPHAN_COLOR,
            44,
            None,
            commit.orphan_source.clone(),
        ));
    }

    children.push(text(summary));
    children.push(spacer());
    children.push(text(format!("{} · {}", commit.author, when)).muted());

    let row_el = row(children)
        .key(format!("commit:{idx}"))
        .focusable()
        .gap(tokens::SPACE_3)
        .padding(Sides::xy(tokens::SPACE_2, 0.0))
        .height(Size::Fixed(ROW_HEIGHT))
        .align(Align::Center);

    if selected { row_el.selected() } else { row_el }
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
    idx: usize,
    selected: bool,
) -> El {
    let amber = tokens::WARNING;
    let lane = layout.map(|l| l.lane).unwrap_or(0);

    let mut children: Vec<El> = vec![graph_cell(lane, amber, selected)];
    if let Some(name) = commit.synthetic_wt_name.as_deref() {
        children.push(pill(
            format!("WT: {name}"),
            amber,
            40,
            Some(format!("worktree:{name}")),
            Some("Switch to this worktree".to_string()),
        ));
    }
    children.push(
        text(if commit.summary.is_empty() {
            "Uncommitted changes".to_string()
        } else {
            commit.summary.clone()
        })
        .text_color(amber),
    );
    children.push(spacer());
    children.push(text(commit.relative_time()).muted().caption());

    row(children)
        .key(format!("commit:{idx}"))
        .focusable()
        .gap(tokens::SPACE_3)
        .padding(Sides::xy(tokens::SPACE_2, 0.0))
        .height(Size::Fixed(ROW_HEIGHT))
        .align(Align::Center)
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
        .gap(tokens::SPACE_2)
        .padding(tokens::SPACE_4)
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
    // closure needs — Vec<CommitInfo> + Vec<CommitLayout> + parallel
    // pill data. The layout / pill lookups go through flat parallel
    // Vecs instead of HashMaps so the closure stays Send + Sync.
    let layouts: Vec<Option<CommitLayout>> = tab
        .commits
        .iter()
        .map(|c| tab.graph_layout.get(&c.id).cloned())
        .collect();
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
    let commits = tab.commits.clone();
    let selected_oid = tab.selected_commit;

    card([
        card_header([row([text(header_text).caption().muted()])])
            .padding(Sides::xy(tokens::SPACE_3, tokens::SPACE_2))
            .fill(tokens::MUTED),
        card_content([virtual_list(commits.len(), ROW_HEIGHT, move |i| {
            let c = &commits[i];
            let selected = selected_oid == Some(c.id);
            build_row(
                c,
                layouts[i].as_ref(),
                &pills_per_row[i],
                ci_per_row[i].as_deref(),
                detached_flags[i],
                i,
                selected,
            )
        })
        .key("commits")
        .height(Size::Fill(1.0))])
        .padding(0.0)
        .height(Size::Fill(1.0)),
    ])
    .width(Size::Fill(1.0))
    .height(Size::Fill(1.0))
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

