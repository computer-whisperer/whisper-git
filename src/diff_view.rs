//! Diff viewer — takes over the center pane when the user picks a
//! file (in the staging well or in a selected commit's file list).
//!
//! Two source modes, picked by `tab.selected_commit`:
//! - `None` → working-tree diff: `repo.diff_working_file(path, staged)`,
//!   with per-hunk Stage / Unstage actions. Whether the *staged* or
//!   *unstaged* side is shown depends on which list the file lives in
//!   (the user toggles by clicking Stage / Unstage on individual hunks).
//! - `Some(oid)` → commit diff: `repo.diff_file_in_commit(oid, path)`,
//!   read-only (no Stage / Unstage — the commit is already history).

use aetna_core::{El, prelude::*};

use crate::git::{DiffHunk, DiffLine, FileStatus};
use crate::repo_tab::{RepoTab, WorktreeView};

pub fn diff_view(tab: &RepoTab) -> El {
    // The body layout only swaps the center to diff_view when active_view
    // is Some and selected_diff_file is Some — but we still defend in
    // depth so a stale build() call doesn't panic.
    let Some(view) = tab.active_view() else {
        return empty_diff("No active worktree.");
    };
    let Some(path) = view.selected_diff_file.as_deref() else {
        return empty_diff("No file selected.");
    };

    if let Some(oid) = tab.selected_commit {
        commit_diff(view, oid, path)
    } else {
        working_diff(view, path)
    }
}

fn working_diff(view: &WorktreeView, path: &str) -> El {
    let staged = file_is_staged(view, path);
    let hunks = view.repo.diff_working_file(path, staged).unwrap_or_default();
    let badge_label = if staged { "staged" } else { "unstaged" };
    diff_card(path, badge_label, &hunks, true, staged)
}

fn commit_diff(view: &WorktreeView, oid: git2::Oid, path: &str) -> El {
    // diff_file_in_commit returns Vec<DiffFile>; the first (and only)
    // file's hunks are the ones we want.
    let files = view.repo.diff_file_in_commit(oid, path).unwrap_or_default();
    let hunks: Vec<DiffHunk> = files.into_iter().flat_map(|f| f.hunks).collect();
    let short = oid.to_string()[..7].to_string();
    diff_card(path, &short, &hunks, false, false)
}

fn diff_card(path: &str, badge_label: &str, hunks: &[DiffHunk], stage_actions: bool, staged: bool) -> El {
    let header_row = row([
        text(path.to_string()).label(),
        spacer(),
        badge(badge_label.to_string()).muted(),
    ])
    .gap(tokens::SPACE_2)
    .align(Align::Center);

    let body: El = if hunks.is_empty() {
        column([text("(no changes)").caption().muted()]).padding(tokens::SPACE_4)
    } else {
        let rows: Vec<El> = hunks
            .iter()
            .enumerate()
            .map(|(idx, h)| hunk_block(h, idx, path, stage_actions, staged))
            .collect();
        column(rows).gap(tokens::SPACE_3).padding(tokens::SPACE_3)
    };

    card([
        card_header([header_row])
            .padding(Sides::xy(tokens::SPACE_4, tokens::SPACE_2))
            .fill(tokens::MUTED),
        card_content([scroll([body])
            .key("diff:scroll")
            .height(Size::Fill(1.0))])
        .padding(0.0)
        .height(Size::Fill(1.0)),
    ])
    .height(Size::Fill(1.0))
    .width(Size::Fill(1.0))
}

fn empty_diff(msg: &str) -> El {
    column([text(msg.to_string()).muted()])
        .align(Align::Center)
        .justify(Justify::Center)
        .padding(tokens::SPACE_4)
        .height(Size::Fill(1.0))
        .width(Size::Fill(1.0))
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

fn hunk_block(hunk: &DiffHunk, idx: usize, path: &str, stage_actions: bool, staged: bool) -> El {
    let mut header_children: Vec<El> = vec![
        text(hunk.header.trim().to_string())
            .code()
            .text_color(tokens::INFO),
        spacer(),
    ];
    if stage_actions {
        let action_label = if staged { "Unstage" } else { "Stage" };
        let action_key = if staged {
            format!("unstage_hunk:{idx}:{path}")
        } else {
            format!("stage_hunk:{idx}:{path}")
        };
        header_children.push(
            button(action_label.to_string())
                .key(action_key)
                .ghost()
                .tooltip(format!("{action_label} this hunk")),
        );
    }
    let header_row = row(header_children).gap(tokens::SPACE_2).align(Align::Center);

    let lines: Vec<El> = hunk.lines.iter().map(line_row).collect();

    card([
        card_header([header_row])
            .padding(Sides::xy(tokens::SPACE_2, tokens::SPACE_1))
            .fill(tokens::ACCENT),
        card_content(lines).padding(0.0),
    ])
}

fn line_row(line: &DiffLine) -> El {
    let color = match line.origin {
        '+' => tokens::SUCCESS,
        '-' => tokens::DESTRUCTIVE,
        _ => tokens::FOREGROUND,
    };
    let prefix = match line.origin {
        '+' | '-' | ' ' => line.origin,
        _ => ' ',
    };
    let new_no = line.new_lineno.map(|n| n.to_string()).unwrap_or_default();
    let old_no = line.old_lineno.map(|n| n.to_string()).unwrap_or_default();

    row([
        text(format!("{old_no:>4}"))
            .mono()
            .caption()
            .muted()
            .nowrap_text()
            .width(Size::Fixed(40.0)),
        text(format!("{new_no:>4}"))
            .mono()
            .caption()
            .muted()
            .nowrap_text()
            .width(Size::Fixed(40.0)),
        text(format!("{prefix} {}", line.content))
            .mono()
            .nowrap_text()
            .text_color(color),
    ])
    .gap(tokens::SPACE_1)
    .padding(Sides::xy(tokens::SPACE_2, 0.0))
}
