//! Staging well: commit-message editor + staged / unstaged file lists.
//!
//! Phase 4a: visual shell + controlled text inputs. Click handlers emit
//! placeholder events that route through `on_event`; real stage / commit
//! ops are wired in Phase 4c.
//!
//! Item keys:
//! - `subject` / `body` — text input / area
//! - `commit` — primary commit button
//! - `stage_all` / `unstage_all` — bulk-op buttons
//! - `stage_file:{path}` / `unstage_file:{path}` — per-file toggle
//! - `diff:{path}` — preview file's diff

use aetna_core::{El, IconName, Selection, prelude::*};

use crate::git::{FileStatus, FileStatusKind};
use crate::repo_tab::{RepoTab, WorktreeView};

pub const STAGING_WIDTH: f32 = 420.0;

/// Pill bar for picking which worktree the staging well operates on.
///
/// Hidden when there's only one worktree (the bar would have nothing to
/// switch between, just clutter). Each pill carries:
/// - the worktree's display name (shortened against the common prefix
///   of all worktree names — e.g. `feat/x` and `feat/y` show as `x` and
///   `y` rather than burning width on the shared prefix)
/// - a dirty-count badge when the worktree's libgit2 metadata reports
///   one
/// - a `.current()` chainable on the active pill so the style profile
///   system paints it as page-current rather than just selected
///
/// Pills are routed under the `wt_select:{path}` key — `ui_app.rs`
/// strips that prefix and calls `RepoTab::select_worktree` with the
/// resolved path. Paths are used as the routing key (rather than
/// names) since names aren't unique across linked worktrees if you
/// have nested setups.
pub fn worktree_selector(tab: &RepoTab) -> Option<El> {
    if !tab.has_worktree_selector() {
        return None;
    }
    let names: Vec<String> = tab
        .worktree_order
        .iter()
        .filter_map(|p| tab.worktree_views.get(p).map(|v| v.name.clone()))
        .collect();
    let display = compute_display_names(&names);
    let active = tab.active_worktree.clone();

    let pills: Vec<El> = tab
        .worktree_order
        .iter()
        .enumerate()
        .filter_map(|(i, path)| {
            let view = tab.worktree_views.get(path)?;
            let label = display.get(i).cloned().unwrap_or_else(|| view.name.clone());
            let dirty = view.status.unstaged.len()
                + view.status.untracked.len()
                + view.status.staged.len()
                + view.status.conflicted.len();
            let is_active = active.as_ref() == Some(path);
            let mut children: Vec<El> = vec![
                icon(IconName::LayoutDashboard).muted(),
                text(label).label(),
            ];
            if dirty > 0 {
                children.push(badge(format!("{dirty}")).muted());
            }
            let key = format!("wt_select:{}", path.to_string_lossy());
            let pill = row(children)
                .key(key)
                .focusable()
                .padding(Sides::xy(tokens::SPACE_2, tokens::SPACE_1))
                .gap(tokens::SPACE_1)
                .align(Align::Center);
            Some(if is_active { pill.current() } else { pill })
        })
        .collect();

    // The pill bar is intrinsically one row tall — without an explicit
    // height the wrapping `scroll` greedily expands to fill the staging
    // pane and the commit box ends up shoved to the bottom.
    Some(
        row(pills)
            .padding(Sides::xy(tokens::SPACE_2, tokens::SPACE_1))
            .gap(tokens::SPACE_1)
            .align(Align::Center)
            .fill(tokens::MUTED)
            .stroke(tokens::BORDER)
            .width(Size::Fill(1.0)),
    )
}

/// Strip the longest common prefix (up to the last separator: `-`,
/// `_`, or `/`) from a list of worktree names so pill labels read
/// short. Single-name lists pass through; if shortening would
/// produce an empty label, the originals are returned.
fn compute_display_names(names: &[String]) -> Vec<String> {
    if names.len() < 2 {
        return names.to_vec();
    }
    let first = &names[0];
    let prefix_len = first.len().min(
        names[1..]
            .iter()
            .map(|n| {
                first
                    .chars()
                    .zip(n.chars())
                    .take_while(|(a, b)| a == b)
                    .count()
            })
            .min()
            .unwrap_or(0),
    );
    let common = &first[..prefix_len];
    let strip_len = common
        .rfind(['-', '_', '/'])
        .map(|i| i + 1)
        .unwrap_or(0);
    if strip_len == 0 {
        return names.to_vec();
    }
    let result: Vec<String> = names.iter().map(|n| n[strip_len..].to_string()).collect();
    if result.iter().any(|s| s.is_empty()) {
        names.to_vec()
    } else {
        result
    }
}

pub fn staging_well(view: &WorktreeView, selection: &Selection) -> El {
    let staged = &view.status.staged;
    let unstaged_all: Vec<&FileStatus> = view
        .status
        .unstaged
        .iter()
        .chain(view.status.untracked.iter())
        .collect();
    let conflicted = &view.status.conflicted;

    let mut sections: Vec<El> = Vec::new();
    sections.push(commit_message(view, selection));
    if !conflicted.is_empty() {
        sections.push(file_section(
            "Conflicted",
            conflicted.iter().collect::<Vec<_>>().as_slice(),
            None,
            SurfaceRole::Danger,
        ));
    }
    sections.push(file_section(
        "Staged",
        staged.iter().collect::<Vec<_>>().as_slice(),
        Some(("Unstage all", "unstage_all", false)),
        SurfaceRole::Sunken,
    ));
    sections.push(file_section(
        "Unstaged",
        unstaged_all.as_slice(),
        Some(("Stage all", "stage_all", true)),
        SurfaceRole::Sunken,
    ));

    column(sections)
        .width(Size::Fixed(STAGING_WIDTH))
        .height(Size::Fill(1.0))
        .padding(tokens::SPACE_3)
        .gap(tokens::SPACE_3)
}

fn commit_message(view: &WorktreeView, selection: &Selection) -> El {
    card([
        card_header([row([
            text("Commit").label(),
            spacer(),
            text(format!("{} staged", view.status.staged.len()))
                .caption()
                .muted(),
        ])
        .align(Align::Center)
        .gap(tokens::SPACE_2)])
        .padding(Sides {
            left: tokens::SPACE_3,
            right: tokens::SPACE_3,
            top: tokens::SPACE_3,
            bottom: tokens::SPACE_2,
        }),
        card_content([
            text_input(&view.commit_subject, selection, "subject")
                .key("subject")
                .width(Size::Fill(1.0)),
            text_area(&view.commit_body, selection, "body")
                .key("body")
                .width(Size::Fill(1.0))
                .height(Size::Fixed(120.0)),
        ])
        .padding(Sides::xy(tokens::SPACE_3, 0.0))
        .gap(tokens::SPACE_2),
        card_footer([
            spacer(),
            button_with_icon(IconName::GitCommit, "Commit")
                .key("commit")
                .primary()
                .tooltip("Stage and commit (Ctrl+Enter)"),
        ])
        .padding(tokens::SPACE_3),
    ])
    .fill(tokens::ACCENT)
}

fn file_section(
    title: &str,
    files: &[&FileStatus],
    bulk_action: Option<(&str, &str, bool)>,
    role: SurfaceRole,
) -> El {
    let is_danger = role == SurfaceRole::Danger;
    let title_el = if is_danger {
        text(title.to_string())
            .caption()
            .text_color(tokens::DESTRUCTIVE)
    } else {
        text(title.to_string()).caption().muted()
    };
    let mut header_children: Vec<El> =
        vec![title_el, badge(files.len().to_string()).muted(), spacer()];
    if let Some((label, key, _is_stage)) = bulk_action
        && !files.is_empty()
    {
        header_children.push(button(label.to_string()).key(key.to_string()).ghost());
    }
    let header_row = row(header_children)
        .align(Align::Center)
        .gap(tokens::SPACE_2);

    let body: Vec<El> = if files.is_empty() {
        vec![
            text("(none)")
                .caption()
                .muted()
                .padding(Sides::xy(tokens::SPACE_4, tokens::SPACE_1)),
        ]
    } else {
        files
            .iter()
            .map(|f| file_row(f, bulk_action.is_some_and(|(_, _, is_stage)| is_stage)))
            .collect()
    };

    let header_fill = if is_danger {
        tokens::DESTRUCTIVE.with_alpha(40)
    } else {
        tokens::MUTED
    };

    let mut card_el = card([
        card_header([header_row])
            .padding(Sides::xy(tokens::SPACE_3, tokens::SPACE_1))
            .fill(header_fill),
        card_content(body).padding(0.0).gap(0.0),
    ]);
    if is_danger {
        card_el = card_el.surface_role(SurfaceRole::Danger);
    }
    card_el
}

fn file_row(file: &FileStatus, is_unstaged_section: bool) -> El {
    let (status_char, status_color) = match file.status {
        FileStatusKind::New => ('A', tokens::SUCCESS),
        FileStatusKind::Modified => ('M', tokens::WARNING),
        FileStatusKind::Deleted => ('D', tokens::DESTRUCTIVE),
        FileStatusKind::Renamed => ('R', tokens::INFO),
        FileStatusKind::TypeChange => ('T', tokens::INFO),
        FileStatusKind::Conflicted => ('!', tokens::DESTRUCTIVE),
    };
    let toggle_key = if is_unstaged_section {
        format!("stage_file:{}", file.path)
    } else {
        format!("unstage_file:{}", file.path)
    };

    row([
        text(status_char.to_string())
            .mono()
            .text_color(status_color),
        text(file.path.clone()),
        spacer(),
        icon_button(if is_unstaged_section {
            IconName::Plus
        } else {
            IconName::X
        })
        .key(toggle_key)
        .tooltip(if is_unstaged_section {
            "Stage file"
        } else {
            "Unstage file"
        }),
    ])
    .key(format!("diff:{}", file.path))
    .focusable()
    .padding(Sides::xy(tokens::SPACE_3, tokens::SPACE_1))
    .gap(tokens::SPACE_2)
    .align(Align::Center)
}
