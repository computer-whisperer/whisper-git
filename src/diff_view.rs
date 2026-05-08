//! Working-directory diff viewer.
//!
//! Phase 4b: renders unified-diff hunks with green/red/info coloring,
//! per-hunk Stage / Unstage buttons. Word-level intra-line highlighting
//! is deferred to polish; the underlying `DiffLine.highlight_ranges`
//! data is already populated by `git/diff.rs`.

use aetna_core::{El, prelude::*};

use crate::git::{DiffHunk, DiffLine, FileStatus};
use crate::repo_tab::RepoTab;

pub fn diff_view(tab: &RepoTab) -> El {
    let Some(path) = tab.selected_diff_file.as_deref() else {
        return placeholder();
    };

    // Show staged hunks if the file lives in the staged list; otherwise
    // show the unstaged side of the diff. (A file can be in both — the
    // user toggles via the diff hunk Stage/Unstage buttons.)
    let staged = file_is_staged(tab, path);
    let hunks = tab.repo.diff_working_file(path, staged).unwrap_or_default();

    let mut header_children: Vec<El> = vec![
        text(path.to_string()).label(),
        spacer(),
        badge(if staged { "staged" } else { "unstaged" }).muted(),
    ];
    let header = row(header_children.drain(..))
        .surface_role(SurfaceRole::Panel)
        .padding(Sides::xy(tokens::SPACE_LG, tokens::SPACE_SM))
        .gap(tokens::SPACE_SM)
        .align(Align::Center);

    let body: El = if hunks.is_empty() {
        column([text("(no changes)").caption().muted()]).padding(tokens::SPACE_LG)
    } else {
        let rows: Vec<El> = hunks
            .iter()
            .enumerate()
            .map(|(idx, h)| hunk_block(h, idx, path, staged))
            .collect();
        column(rows).gap(tokens::SPACE_MD).padding(tokens::SPACE_MD)
    };

    column([
        header,
        scroll([body]).key("diff:scroll").height(Size::Fill(1.0)),
    ])
    .gap(0.0)
    .height(Size::Fill(1.0))
    .width(Size::Fill(1.0))
    .surface_role(SurfaceRole::Panel)
}

fn placeholder() -> El {
    column([
        h2("No diff selected"),
        paragraph(
            "Click a file in the staging well to preview its diff. \
             Phase 4c will wire Stage / Unstage hunks.",
        ),
    ])
    .padding(tokens::SPACE_LG)
    .gap(tokens::SPACE_MD)
    .height(Size::Fill(1.0))
    .width(Size::Fill(1.0))
}

fn file_is_staged(tab: &RepoTab, path: &str) -> bool {
    if tab
        .status
        .staged
        .iter()
        .any(|f: &FileStatus| f.path == path)
    {
        // If it's *also* in unstaged, prefer unstaged (where the user is
        // actively editing). Otherwise show the staged side.
        !tab.status.unstaged.iter().any(|f| f.path == path)
            && !tab.status.untracked.iter().any(|f| f.path == path)
    } else {
        false
    }
}

fn hunk_block(hunk: &DiffHunk, idx: usize, path: &str, staged: bool) -> El {
    let action_label = if staged { "Unstage" } else { "Stage" };
    let action_key = if staged {
        format!("unstage_hunk:{idx}:{path}")
    } else {
        format!("stage_hunk:{idx}:{path}")
    };

    let header = row([
        text(hunk.header.trim().to_string())
            .code()
            .text_color(tokens::INFO),
        spacer(),
        button(action_label.to_string())
            .key(action_key)
            .ghost()
            .tooltip(format!("{action_label} this hunk")),
    ])
    .padding(Sides::xy(tokens::SPACE_SM, tokens::SPACE_XS))
    .gap(tokens::SPACE_SM)
    .align(Align::Center)
    .surface_role(SurfaceRole::Raised);

    let lines: Vec<El> = hunk.lines.iter().map(line_row).collect();

    column([header, column(lines).gap(0.0)])
        .gap(0.0)
        .surface_role(SurfaceRole::Panel)
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
    .gap(tokens::SPACE_XS)
    .padding(Sides::xy(tokens::SPACE_SM, 0.0))
}
