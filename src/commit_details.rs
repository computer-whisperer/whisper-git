//! Right-pane upper section when a commit is selected: full SHA +
//! parents, author + timestamp, full commit message, and the list of
//! changed files with per-file insertion / deletion stats.
//!
//! Mirrors the staging well's "stack of cards in a plain scroll"
//! shape so the right pane has one consistent rhythm regardless of
//! whether it's showing draft state or browse state. The data is
//! cached on `RepoTab::commit_detail` so opening the pane doesn't
//! rerun libgit2 every frame.

use aetna_core::{El, IconName, prelude::*};

use crate::repo_tab::RepoTab;

pub fn commit_details_pane(tab: &RepoTab) -> El {
    let pane = match (tab.selected_commit, &tab.commit_detail) {
        (Some(_), Some(detail)) => details_pane(detail),
        (Some(_), None) => placeholder_pane("Loading…"),
        (None, _) => placeholder_pane("Select a commit to inspect."),
    };
    pane.height(Size::Fill(1.0))
}

fn placeholder_pane(msg: &str) -> El {
    column([text(msg.to_string()).muted()])
        .align(Align::Center)
        .justify(Justify::Center)
        .padding(tokens::SPACE_4)
        .height(Size::Fill(1.0))
        .width(Size::Fill(1.0))
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

    let identity_card = card([card_header([
        row([
            icon(IconName::GitCommit),
            // .label() resets font_mono to false (label is the
            // "field label" role and intentionally proportional).
            // Chain .mono() after so the SHA renders in JBM.
            text(info.short_id.clone()).label().mono(),
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
    .padding(tokens::SPACE_3)
    .gap(tokens::SPACE_1)]);

    // Compose by hand instead of `titled_card` so the subject can
    // ellipsis when it overflows. `card_title` defaults to `.hug()`
    // sizing, which lets a long subject blow past the card edge
    // (`TextOverflow` lint at the heading).
    let subject_card = card([
        card_header([card_title(subject).width(Size::Fill(1.0)).ellipsis()]),
        card_content(body_children),
    ]);
    let mut cards: Vec<El> = vec![identity_card, subject_card, files_card(detail)];
    if !detail.submodule_entries.is_empty() {
        cards.push(submodules_card(detail));
    }

    scroll([column(cards).gap(tokens::SPACE_3).padding(tokens::SPACE_3)])
        .key("commit_details:scroll")
        .height(Size::Fill(1.0))
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

/// "Submodules (N changed)" card listing the pinned SHA at this
/// commit for each registered submodule. Changed entries surface as
/// `parent_short \u{2192} pinned_short` so reviewers can see what
/// pointer move the commit made; new submodules show just the pinned
/// SHA. Rows are click-routed under `submodule:open:<path>` —
/// drill-down (Phase 4) wires the route.
fn submodules_card(detail: &crate::repo_tab::CommitDetail) -> El {
    let entries = &detail.submodule_entries;
    let changed_count = entries.iter().filter(|e| e.changed).count();
    let title = if changed_count > 0 {
        format!("Submodules ({changed_count} changed)")
    } else {
        format!("Submodules ({})", entries.len())
    };

    let summary = row([text(title).label(), spacer()])
        .gap(tokens::SPACE_2)
        .align(Align::Center);

    let body: Vec<El> = entries.iter().map(submodule_entry_row).collect();

    card([
        card_header([summary]).padding(tokens::SPACE_3),
        card_content(body)
            .padding(tokens::SPACE_3)
            .pt(0.0)
            .gap(tokens::SPACE_1),
    ])
}

fn submodule_entry_row(entry: &crate::git::CommitSubmoduleEntry) -> El {
    let pinned = entry.pinned_oid.to_string();
    let pinned_short = &pinned[..7];
    let sha_el = if entry.changed {
        match entry.parent_oid {
            Some(parent) => {
                let parent_str = parent.to_string();
                let parent_short = &parent_str[..7];
                row([
                    text(parent_short.to_string()).mono().muted(),
                    text(" \u{2192} ".to_string()).mono().muted(),
                    text(pinned_short.to_string())
                        .mono()
                        .text_color(tokens::WARNING),
                ])
                .gap(0.0)
                .align(Align::Center)
            }
            None => text(format!("new \u{00b7} {pinned_short}"))
                .mono()
                .text_color(tokens::SUCCESS),
        }
    } else {
        text(pinned_short.to_string()).mono().muted()
    };

    let name_el = if entry.changed {
        text(entry.name.clone()).text_color(tokens::WARNING)
    } else {
        text(entry.name.clone())
    };

    row([
        icon(IconName::Folder).muted(),
        name_el.nowrap_text(),
        spacer(),
        sha_el,
    ])
    .key(format!("submodule:open:{}", entry.path))
    .focusable()
    .style_profile(StyleProfile::Surface)
    .metrics_role(MetricsRole::ListItem)
    .cursor(Cursor::Pointer)
    .paint_overflow(Sides::all(tokens::RING_WIDTH))
    .radius(tokens::RADIUS_SM)
    .animate(Timing::SPRING_QUICK)
    .padding(Sides::xy(tokens::SPACE_2, tokens::SPACE_1))
    .gap(tokens::SPACE_2)
    .align(Align::Center)
    .height(Size::Fixed(28.0))
    .tooltip(format!("path: {}", entry.path))
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
