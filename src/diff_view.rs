//! Adapter from whisper-git's [`RepoTab`] state to the
//! [`crate::widgets::diff`] widget. Picks working-tree vs commit-diff
//! source off `tab.selected_commit`; converts libgit2's
//! `git::DiffHunk` / `git::DiffLine` into the widget's pure
//! data types; routes per-hunk Stage / Unstage keys.

use aetna_core::{El, prelude::*};

use crate::git::{self, FileStatus};
use crate::repo_tab::{RepoTab, WorktreeView};
use crate::widgets::diff::{
    DiffData, DiffHunk, DiffHunkAction, DiffLine, DiffLineKind, DiffMode, diff,
};

pub const DIFF_MODE_TOGGLE_KEY: &str = "diff:mode_toggle";

pub fn diff_view(tab: &RepoTab, mode: DiffMode) -> El {
    let Some(view) = tab.active_view() else {
        return empty_diff("No active worktree.");
    };
    let Some(path) = view.selected_diff_file.as_deref() else {
        return empty_diff("No file selected.");
    };

    let mut data = if let Some(oid) = tab.selected_commit {
        commit_diff(view, oid, path)
    } else {
        working_diff(view, path)
    };
    data.mode = mode;
    data.mode_toggle_key = Some(DIFF_MODE_TOGGLE_KEY.to_string());
    diff(&data)
}

fn working_diff(view: &WorktreeView, path: &str) -> DiffData {
    let staged = file_is_staged(view, path);
    let hunks = view
        .repo
        .diff_working_file(path, staged)
        .unwrap_or_default();
    let badge = if staged { "staged" } else { "unstaged" }.to_string();
    let widget_hunks: Vec<DiffHunk> = hunks
        .into_iter()
        .enumerate()
        .map(|(idx, h)| convert_hunk(h, working_action(idx, path, staged)))
        .collect();
    DiffData {
        title: path.to_string(),
        badge: Some(badge),
        hunks: widget_hunks,
        mode: DiffMode::Unified,
        mode_toggle_key: None,
    }
}

fn commit_diff(view: &WorktreeView, oid: git2::Oid, path: &str) -> DiffData {
    let files = view.repo.diff_file_in_commit(oid, path).unwrap_or_default();
    let widget_hunks: Vec<DiffHunk> = files
        .into_iter()
        .flat_map(|f| f.hunks)
        // No per-hunk Stage / Unstage in commit context — the commit is
        // already history.
        .map(|h| convert_hunk(h, None))
        .collect();
    let short = oid.to_string()[..7].to_string();
    DiffData {
        title: path.to_string(),
        badge: Some(short),
        hunks: widget_hunks,
        mode: DiffMode::Unified,
        mode_toggle_key: None,
    }
}

fn convert_hunk(hunk: git::DiffHunk, action: Option<DiffHunkAction>) -> DiffHunk {
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
        action,
    }
}

fn working_action(idx: usize, path: &str, staged: bool) -> Option<DiffHunkAction> {
    let (label, key, tip) = if staged {
        (
            "Unstage",
            format!("unstage_hunk:{idx}:{path}"),
            "Unstage this hunk",
        )
    } else {
        (
            "Stage",
            format!("stage_hunk:{idx}:{path}"),
            "Stage this hunk",
        )
    };
    Some(DiffHunkAction {
        label: label.to_string(),
        key,
        tooltip: Some(tip.to_string()),
    })
}

fn file_is_staged(view: &WorktreeView, path: &str) -> bool {
    if view
        .status
        .staged
        .iter()
        .any(|f: &FileStatus| f.path == path)
    {
        // If it's *also* in unstaged, prefer unstaged (where the user is
        // actively editing). Otherwise show the staged side.
        !view.status.unstaged.iter().any(|f| f.path == path)
            && !view.status.untracked.iter().any(|f| f.path == path)
    } else {
        false
    }
}

fn empty_diff(msg: &str) -> El {
    column([text(msg.to_string()).muted()])
        .align(Align::Center)
        .justify(Justify::Center)
        .padding(tokens::SPACE_4)
        .height(Size::Fill(1.0))
        .width(Size::Fill(1.0))
}
