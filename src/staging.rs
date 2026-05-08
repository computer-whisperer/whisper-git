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
use crate::repo_tab::RepoTab;

pub const STAGING_WIDTH: f32 = 420.0;

pub fn staging_well(tab: &RepoTab, selection: &Selection) -> El {
    let staged = &tab.status.staged;
    let unstaged_all: Vec<&FileStatus> = tab
        .status
        .unstaged
        .iter()
        .chain(tab.status.untracked.iter())
        .collect();
    let conflicted = &tab.status.conflicted;

    let mut sections: Vec<El> = Vec::new();
    sections.push(commit_message(tab, selection));
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

    card([card_content(sections)
        .gap(0.0)
        .padding(0.0)
        .height(Size::Fill(1.0))])
    .width(Size::Fixed(STAGING_WIDTH))
    .height(Size::Fill(1.0))
}

fn commit_message(tab: &RepoTab, selection: &Selection) -> El {
    column([
        row([
            text("Commit").label(),
            spacer(),
            text(format!("{} staged", tab.status.staged.len()))
                .caption()
                .muted(),
        ])
        .align(Align::Center)
        .gap(tokens::SPACE_SM),
        text_input(&tab.commit_subject, selection, "subject")
            .key("subject")
            .width(Size::Fill(1.0)),
        text_area(&tab.commit_body, selection, "body")
            .key("body")
            .width(Size::Fill(1.0))
            .height(Size::Fixed(120.0)),
        row([
            spacer(),
            button_with_icon(IconName::GitCommit, "Commit")
                .key("commit")
                .primary()
                .tooltip("Stage and commit (Ctrl+Enter)"),
        ])
        .align(Align::Center),
    ])
    .padding(tokens::SPACE_MD)
    .gap(tokens::SPACE_SM)
    .fill(tokens::ACCENT)
    .stroke(tokens::BORDER)
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
    let header = row(header_children)
        .align(Align::Center)
        .gap(tokens::SPACE_SM)
        .padding(Sides::xy(tokens::SPACE_MD, tokens::SPACE_XS))
        .fill(if is_danger {
            tokens::DESTRUCTIVE.with_alpha(40)
        } else {
            tokens::MUTED
        })
        .stroke(tokens::BORDER);

    let body: Vec<El> = if files.is_empty() {
        vec![
            text("(none)")
                .caption()
                .muted()
                .padding(Sides::xy(tokens::SPACE_LG, tokens::SPACE_XS)),
        ]
    } else {
        files
            .iter()
            .map(|f| file_row(f, bulk_action.is_some_and(|(_, _, is_stage)| is_stage)))
            .collect()
    };

    column([header, column(body).gap(0.0)]).gap(0.0)
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
    .padding(Sides::xy(tokens::SPACE_MD, tokens::SPACE_XS))
    .gap(tokens::SPACE_SM)
    .align(Align::Center)
}
