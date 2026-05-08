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
    let body = match (tab.selected_commit, &tab.commit_detail) {
        (Some(_), Some(detail)) => details_body(detail),
        (Some(_), None) => placeholder("Loading…"),
        (None, _) => placeholder("Select a commit to inspect."),
    };

    column([body])
        .width(Size::Fixed(PANE_WIDTH))
        .height(Size::Fill(1.0))
        .fill(tokens::CARD)
        .stroke(tokens::BORDER)
}

fn placeholder(msg: &str) -> El {
    column([text(msg.to_string()).muted()])
        .padding(tokens::SPACE_LG)
        .gap(tokens::SPACE_SM)
        .width(Size::Fill(1.0))
        .height(Size::Fill(1.0))
        .align(Align::Center)
}

fn details_body(detail: &crate::repo_tab::CommitDetail) -> El {
    let info = &detail.info;
    let parents_label = if info.parent_short_ids.is_empty() {
        "(root commit)".to_string()
    } else {
        format!("Parents: {}", info.parent_short_ids.join(", "))
    };

    let (subject, body) = split_message(&info.full_message);
    let body_el: Option<El> = if body.trim().is_empty() {
        None
    } else {
        Some(paragraph(body).label())
    };

    let header = column([
        row([
            icon(IconName::GitCommit),
            text(info.short_id.clone()).mono().label(),
            spacer(),
            button("Copy SHA")
                .key("details:copy_sha")
                .ghost()
                .tooltip("Copy full commit SHA"),
        ])
        .gap(tokens::SPACE_SM)
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
    .gap(tokens::SPACE_XS);

    let mut message_children: Vec<El> = vec![h3(subject)];
    if let Some(b) = body_el {
        message_children.push(b);
    }
    let message_card = card([column(message_children).gap(tokens::SPACE_SM)]);

    let files_card = card([column(files_section(detail)).gap(tokens::SPACE_XS)]);

    scroll([column([header, message_card, files_card])
        .gap(tokens::SPACE_MD)
        .padding(tokens::SPACE_LG)])
    .width(Size::Fill(1.0))
    .height(Size::Fill(1.0))
}

fn files_section(detail: &crate::repo_tab::CommitDetail) -> Vec<El> {
    let mut out: Vec<El> = Vec::with_capacity(detail.files.len() + 1);
    out.push(
        row([
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
        .gap(tokens::SPACE_SM)
        .align(Align::Center),
    );
    if detail.files.is_empty() {
        out.push(text("No file changes.").muted());
    } else {
        for f in &detail.files {
            out.push(file_row(f));
        }
    }
    out
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
        .gap(tokens::SPACE_SM)
    };
    row([text(f.path.clone()).mono().nowrap_text(), spacer(), stats])
        .gap(tokens::SPACE_SM)
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
