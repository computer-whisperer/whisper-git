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

use crate::git::{FileStatus, FileStatusKind, SubmoduleInfo};
use crate::repo_tab::{RepoTab, WorktreeView};


/// Pill bar for picking which worktree the staging well operates on.
///
/// Hidden when there's only one worktree (the bar would have nothing to
/// switch between, just clutter). The bar is the catalog
/// `tabs_list_from_triggers` — each option is a [`tab_trigger_content`]
/// carrying the worktree's display name (shortened against the common
/// prefix of all worktree names — e.g. `feat/x` and `feat/y` show as
/// `x` and `y` rather than burning width on the shared prefix) plus a
/// dirty-count badge when libgit2 reports one.
///
/// Pills are routed under `wt_select:tab:{path}` — the standard
/// `{list_key}:tab:{value}` shape that aetna's tabs use. `ui_app.rs`
/// strips that prefix and calls `RepoTab::select_worktree` with the
/// resolved path. Paths are the trigger value (rather than names)
/// since names aren't unique across nested linked worktrees.
pub fn worktree_selector(tab: &RepoTab) -> Option<El> {
    use aetna_core::widgets::tabs::{tab_trigger_content, tabs_list_from_triggers};

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

    let triggers: Vec<El> = tab
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
                text(label).label().ellipsis(),
            ];
            if dirty > 0 {
                children.push(badge(format!("{dirty}")).muted());
            }
            Some(tab_trigger_content(
                "wt_select",
                path.to_string_lossy(),
                children,
                is_active,
            ))
        })
        .collect();

    let pills = tabs_list_from_triggers(triggers);
    // Trailing + icon opens the create-worktree modal. The icon_button
    // style matches the tabs_list height so the row reads as one
    // affordance strip rather than two stacked elements.
    let plus = icon_button(IconName::Plus)
        .key("new_worktree")
        .tooltip("Create worktree\u{2026}");
    Some(
        row([pills.width(Size::Fill(1.0)), plus])
            .gap(tokens::SPACE_1)
            .align(Align::Center)
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

pub fn staging_well(view: &WorktreeView, selection: &Selection, ai_in_flight: bool) -> El {
    let staged = &view.status.staged;
    let unstaged_all: Vec<&FileStatus> = view
        .status
        .unstaged
        .iter()
        .chain(view.status.untracked.iter())
        .collect();
    let conflicted = &view.status.conflicted;

    let mut sections: Vec<El> = Vec::new();
    sections.push(commit_message(view, selection, ai_in_flight));
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
    if !view.submodules.is_empty() {
        sections.push(submodules_section(&view.submodules));
    }

    column(sections)
        .width(Size::Fill(1.0))
        .height(Size::Fill(1.0))
        .padding(tokens::SPACE_3)
        .gap(tokens::SPACE_3)
}

fn commit_message(view: &WorktreeView, selection: &Selection, ai_in_flight: bool) -> El {
    let staged_count = view.status.staged.len();
    // Generate is gated on having something to summarize and on no
    // existing in-flight generation. Aetna re-emits the click as
    // long as the button isn't `.disabled()`, so the gate must be
    // visible here — handle_action's runtime gate is just a
    // belt-and-braces backstop for cases where the button slipped
    // through (e.g. keyboard activation).
    let generate_enabled = staged_count > 0 && !ai_in_flight;
    let mut generate_btn = button("Generate")
        .key("ai_generate")
        .ghost()
        .tooltip("Generate commit message via AI");
    if !generate_enabled {
        generate_btn = generate_btn.disabled();
    }
    card([
        card_header([row([
            text("Commit").label(),
            spacer(),
            text(format!("{staged_count} staged"))
                .caption()
                .muted(),
        ])
        .align(Align::Center)
        .gap(tokens::SPACE_2)])
        .padding(tokens::SPACE_3)
        .pb(tokens::SPACE_2),
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
            generate_btn,
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
        card_content(body).padding(0.0),
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
    .style_profile(StyleProfile::Surface)
    .metrics_role(MetricsRole::ListItem)
    .cursor(Cursor::Pointer)
    .paint_overflow(Sides::all(tokens::RING_WIDTH))
    .radius(tokens::RADIUS_SM)
    .animate(Timing::SPRING_QUICK)
    .padding(Sides::xy(tokens::SPACE_3, tokens::SPACE_1))
    .gap(tokens::SPACE_2)
    .align(Align::Center)
    .height(Size::Fixed(40.0))
}

/// Submodules registered in the active worktree. Each row shows the
/// submodule's name + branch + status pill. Rows are click-routed under
/// `submodule:open:<path>` — Phase 4 (drill-down navigation) wires the
/// route; for now the click is informational. The header counts both
/// total submodules and how many show staged-pointer / dirty state so
/// users see at a glance whether there's submodule work pending.
fn submodules_section(submodules: &[SubmoduleInfo]) -> El {
    let pointer_changed = submodules
        .iter()
        .filter(|s| pin_changed(s))
        .count();
    let dirty = submodules
        .iter()
        .filter(|s| s.is_dirty == Some(true))
        .count();

    let mut header_children: Vec<El> = vec![
        text("Submodules").caption().muted(),
        badge(submodules.len().to_string()).muted(),
        spacer(),
    ];
    if pointer_changed > 0 {
        header_children.push(badge(format!("{pointer_changed} staged")).warning());
    }
    if dirty > 0 {
        header_children.push(badge(format!("{dirty} modified")).warning());
    }

    let body: Vec<El> = submodules.iter().map(submodule_row).collect();

    card([
        card_header([row(header_children)
            .align(Align::Center)
            .gap(tokens::SPACE_2)])
        .padding(Sides::xy(tokens::SPACE_3, tokens::SPACE_1))
        .fill(tokens::MUTED),
        card_content(body).padding(0.0),
    ])
}

fn submodule_row(sm: &SubmoduleInfo) -> El {
    let (status_label, status_color) = submodule_status(sm);
    let path_short = sm.path.rsplit('/').next().unwrap_or(&sm.path).to_string();

    let mut row_children: Vec<El> = vec![
        icon(IconName::Folder).muted(),
        text(if sm.name.is_empty() {
            path_short
        } else {
            sm.name.clone()
        }),
    ];
    if !sm.branch.is_empty() && sm.branch != "unknown" {
        row_children.push(text(format!("\u{00b7} {}", sm.branch)).caption().muted());
    }
    row_children.push(spacer());
    if let Some(label) = status_label {
        row_children.push(badge(label).muted().text_color(status_color));
    }

    row(row_children)
        .key(format!("submodule:open:{}", sm.path))
        .focusable()
        .style_profile(StyleProfile::Surface)
        .metrics_role(MetricsRole::ListItem)
        .cursor(Cursor::Pointer)
        .paint_overflow(Sides::all(tokens::RING_WIDTH))
        .radius(tokens::RADIUS_SM)
        .animate(Timing::SPRING_QUICK)
        .padding(Sides::xy(tokens::SPACE_3, tokens::SPACE_1))
        .gap(tokens::SPACE_2)
        .align(Align::Center)
        .height(Size::Fixed(32.0))
        .tooltip(submodule_tooltip(sm))
}

/// Compact status label for one submodule. Returns `(label, color)`,
/// or `(None, color)` for the clean / unknown cases. Priority:
/// modified (working dir dirty) > staged-pointer (index_oid drifts
/// from head_oid) > checkout-drift (workdir_oid differs from
/// head_oid without a corresponding stage) > clean.
fn submodule_status(sm: &SubmoduleInfo) -> (Option<String>, Color) {
    if sm.is_dirty == Some(true) {
        return (Some("modified".to_string()), tokens::WARNING);
    }
    if pin_changed(sm) {
        return (Some("staged".to_string()), tokens::INFO);
    }
    if sm.workdir_oid != sm.head_oid && sm.workdir_oid.is_some() {
        return (Some("drift".to_string()), tokens::DESTRUCTIVE);
    }
    if sm.is_dirty.is_none() {
        // Async dirty check hasn't returned yet — keep the row neutral.
        return (None, tokens::MUTED_FOREGROUND);
    }
    (None, tokens::SUCCESS)
}

fn pin_changed(sm: &SubmoduleInfo) -> bool {
    match (sm.index_oid, sm.head_oid) {
        (Some(idx), Some(head)) => idx != head,
        _ => false,
    }
}

fn submodule_tooltip(sm: &SubmoduleInfo) -> String {
    let head = sm
        .head_oid
        .map(|o| o.to_string()[..7].to_string())
        .unwrap_or_else(|| "?".to_string());
    let mut parts = vec![format!("HEAD pin: {head}")];
    if let Some(idx) = sm.index_oid
        && Some(idx) != sm.head_oid
    {
        parts.push(format!("staged: {}", &idx.to_string()[..7]));
    }
    if let Some(wd) = sm.workdir_oid
        && Some(wd) != sm.head_oid
    {
        parts.push(format!("checked out: {}", &wd.to_string()[..7]));
    }
    parts.push(format!("path: {}", sm.path));
    parts.join("\n")
}
