//! Right-pane preview for the History view: full SHA + parents,
//! author + timestamp, full commit message, and the list of changed
//! files with per-file insertion / deletion stats.
//!
//! The data is cached on `RepoTab::commit_detail` so opening the pane
//! doesn't rerun libgit2 every frame.

use aetna_core::{El, IconName, prelude::*};

use crate::repo_tab::RepoTab;

const PANE_WIDTH: f32 = 420.0;

pub fn commit_details_pane(tab: &RepoTab) -> El {
    let pane = match (tab.selected_commit, &tab.commit_detail) {
        (Some(_), Some(detail)) => details_pane(detail),
        (Some(_), None) => placeholder_pane("Loading…"),
        (None, _) => placeholder_pane("Select a commit to inspect."),
    };
    pane.width(Size::Fixed(PANE_WIDTH)).height(Size::Fill(1.0))
}

fn placeholder_pane(msg: &str) -> El {
    card([
        card_content([text(msg.to_string()).muted()])
            .height(Size::Fill(1.0))
            .align(Align::Center)
            .justify(Justify::Center),
    ])
}

fn details_pane(detail: &crate::repo_tab::CommitDetail) -> El {
    let info = &detail.info;
    let parents_label = if info.parent_short_ids.is_empty() {
        "(root commit)".to_string()
    } else {
        format!("Parents: {}", info.parent_short_ids.join(", "))
    };
    let (subject, body) = split_message(&info.full_message);
    let body_children: Vec<El> = if body.trim().is_empty() {
        Vec::new()
    } else {
        vec![paragraph(body).label()]
    };

    let identity = card_header([
        row([
            icon(IconName::GitCommit),
            text(info.short_id.clone()).mono().label(),
            spacer(),
            button("Copy SHA")
                .key("details:copy_sha")
                .ghost()
                .tooltip("Copy full commit SHA"),
        ])
        .gap(tokens::SPACE_2)
        .align(Align::Center),
        text(parents_label).muted().caption(),
        text(format!(
            "{} <{}> · {}",
            info.author_name,
            info.author_email,
            info.relative_author_time(),
        ))
        .muted(),
    ])
    .padding(tokens::SPACE_4)
    .gap(tokens::SPACE_1);

    let scroll_body = scroll([column([
        titled_card(subject, body_children),
        files_card(detail),
    ])
    .gap(tokens::SPACE_3)
    .padding(tokens::SPACE_4)
    .pt(0.0)])
    .key("commit_details:scroll")
    .height(Size::Fill(1.0));

    card([
        identity,
        card_content([scroll_body])
            .padding(0.0)
            .height(Size::Fill(1.0)),
    ])
}

fn files_card(detail: &crate::repo_tab::CommitDetail) -> El {
    let summary = row([
        text(format!("{} files", detail.files.len())).label(),
        spacer(),
        text(format!(
            "+{}  -{}",
            detail.files.iter().map(|f| f.additions).sum::<usize>(),
            detail.files.iter().map(|f| f.deletions).sum::<usize>(),
        ))
        .mono()
        .muted(),
    ])
    .gap(tokens::SPACE_2)
    .align(Align::Center);

    let body: Vec<El> = if detail.files.is_empty() {
        vec![text("No file changes.").muted()]
    } else {
        detail.files.iter().map(file_row).collect()
    };

    card([
        card_header([summary]).padding(tokens::SPACE_3),
        card_content(body)
            .padding(tokens::SPACE_3)
            .pt(0.0)
            .gap(tokens::SPACE_1),
    ])
}

fn file_row(f: &crate::git::DiffFile) -> El {
    let stats = if f.additions == 0 && f.deletions == 0 {
        text("renamed").mono().muted()
    } else {
        row([
            text(format!("+{}", f.additions))
                .mono()
                .text_color(tokens::SUCCESS),
            text(format!("-{}", f.deletions))
                .mono()
                .text_color(tokens::DESTRUCTIVE),
        ])
        .gap(tokens::SPACE_2)
    };
    row([text(f.path.clone()).mono().nowrap_text(), spacer(), stats])
        .gap(tokens::SPACE_2)
        .align(Align::Center)
        .key(format!("commit_file:{}", f.path))
        .focusable()
}

/// Split a commit message into (subject, body). Body is everything
/// after the first empty line, trimmed.
fn split_message(msg: &str) -> (String, String) {
    let mut lines = msg.lines();
    let subject = lines.next().unwrap_or("").trim().to_string();
    let mut rest = String::new();
    for line in lines {
        rest.push_str(line);
        rest.push('\n');
    }
    (subject, rest.trim().to_string())
}
