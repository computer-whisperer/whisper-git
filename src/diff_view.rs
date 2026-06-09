//! Adapter from whisper-git's [`RepoTab`] state to the
//! [`crate::widgets::diff`] widget. Picks working-tree vs commit-diff
//! source off `tab.selected_commit`; converts libgit2's
//! `git::DiffHunk` / `git::DiffLine` into the widget's pure
//! data types; routes per-hunk Stage / Unstage keys.

use damascene_core::{El, prelude::*};

use crate::git;
use crate::repo_tab::RepoTab;
use crate::widgets::diff::{
    DiffData, DiffHunk, DiffHunkAction, DiffLine, DiffLineKind, DiffMode, diff,
};

pub const DIFF_MODE_TOGGLE_KEY: &str = "diff:mode_toggle";

/// Render the diff pane from the tab's async diff cache. The hunks
/// are computed off-thread by `git_async::spawn_diff_fetch` (driven
/// from `WhisperApp::poll_diff_fetch`); this function never touches
/// libgit2. While a re-fetch for newer content is in flight, the
/// previous hunks for the same target keep rendering
/// (stale-while-revalidate) so edits don't flash a loading state.
pub fn diff_view(tab: &RepoTab, mode: DiffMode) -> El {
    let Some(view) = tab.active_view() else {
        return empty_diff("No active worktree.");
    };
    if view.selected_diff_file.is_none() {
        return empty_diff("No file selected.");
    }
    let Some(desired) = tab.desired_diff_key() else {
        return empty_diff("No file selected.");
    };
    let Some((key, hunks)) = tab
        .diff_cache
        .as_ref()
        .filter(|(k, _)| k.same_target(&desired))
    else {
        return empty_diff("Loading diff…");
    };

    let path = key.file.as_str();
    let (badge, widget_hunks) = if let Some(oid) = key.commit {
        // No per-hunk Stage / Unstage in commit context — the commit
        // is already history.
        let short = oid.to_string()[..7].to_string();
        let converted = hunks
            .iter()
            .cloned()
            .map(|h| convert_hunk(h, Vec::new()))
            .collect();
        (short, converted)
    } else {
        let staged = key.staged;
        let badge = if staged { "staged" } else { "unstaged" }.to_string();
        let converted = hunks
            .iter()
            .cloned()
            .enumerate()
            .map(|(idx, h)| convert_hunk(h, working_actions(idx, path, staged)))
            .collect();
        (badge, converted)
    };

    let data = DiffData {
        title: path.to_string(),
        badge: Some(badge),
        hunks: widget_hunks,
        mode,
        mode_toggle_key: Some(DIFF_MODE_TOGGLE_KEY.to_string()),
    };
    diff(&data)
}

fn convert_hunk(hunk: git::DiffHunk, actions: Vec<DiffHunkAction>) -> DiffHunk {
    let lines: Vec<DiffLine> = hunk
        .lines
        .into_iter()
        .map(|l| DiffLine {
            kind: match l.origin {
                '+' => DiffLineKind::Addition,
                '-' => DiffLineKind::Deletion,
                _ => DiffLineKind::Context,
            },
            content: l.content,
            old_lineno: l.old_lineno,
            new_lineno: l.new_lineno,
            highlights: l.highlight_ranges,
        })
        .collect();
    DiffHunk {
        header: hunk.header,
        lines,
        actions,
    }
}

fn working_actions(idx: usize, path: &str, staged: bool) -> Vec<DiffHunkAction> {
    if staged {
        return vec![DiffHunkAction {
            label: "Unstage".to_string(),
            key: format!("unstage_hunk:{idx}:{path}"),
            tooltip: Some("Unstage this hunk".to_string()),
            destructive: false,
        }];
    }
    vec![
        DiffHunkAction {
            label: "Stage".to_string(),
            key: format!("stage_hunk:{idx}:{path}"),
            tooltip: Some("Stage this hunk".to_string()),
            destructive: false,
        },
        DiffHunkAction {
            label: "Discard".to_string(),
            key: format!("discard_hunk:{idx}:{path}"),
            tooltip: Some("Discard this hunk".to_string()),
            destructive: true,
        },
    ]
}

fn empty_diff(msg: &str) -> El {
    column([text(msg.to_string()).muted()])
        .align(Align::Center)
        .justify(Justify::Center)
        .padding(tokens::SPACE_4)
        .height(Size::Fill(1.0))
        .width(Size::Fill(1.0))
}
