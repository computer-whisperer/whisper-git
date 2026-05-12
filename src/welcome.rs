//! Welcome view — shown when no repo tab is open.
//!
//! Centered hero (logo + title + tagline), then a Open / Clone action
//! row, and a recent-repos column when normalized recent entries are
//! non-empty. Click targets emit:
//!
//! - `open_repo` — reuses the existing open-folder picker
//! - `welcome:clone` — opens the clone modal
//! - `welcome:recent:{idx}` — opens the normalized recent path at that index

use std::sync::LazyLock;

use aetna_core::{El, IconName, SvgIcon, prelude::*};

use crate::recent::RecentRepoEntry;

const HERO_ICON_PX: f32 = 96.0;
const CONTENT_COLUMN_WIDTH: f32 = 560.0;

/// App logo, parsed once on first paint. The asset is multi-tone, so
/// we use `parse` (preserves the SVG's own colors) rather than
/// `parse_current_color`.
static LOGO: LazyLock<SvgIcon> = LazyLock::new(|| {
    SvgIcon::parse(include_str!("../assets/git-client-icon.svg"))
        .expect("git-client-icon.svg failed to parse")
});

pub fn welcome_view(recent: &[RecentRepoEntry]) -> El {
    // Hero spans the full content column so its `align(Center)` actually
    // centers the icon/title/tagline within the 560 px frame instead of
    // hugging them tight together.
    let hero = column([
        icon(&*LOGO).icon_size(HERO_ICON_PX),
        h1("whisper-git"),
        paragraph("GPU-accelerated Git client").muted(),
    ])
    .gap(tokens::SPACE_2)
    .align(Align::Center)
    .width(Size::Fill(1.0));

    // Spacer flanks let the buttons keep their hug widths while
    // centering inside the 560 px row.
    let actions = row([
        spacer(),
        button_with_icon(IconName::Folder, "Open Local\u{2026}")
            .key("open_repo")
            .primary(),
        button_with_icon(IconName::Download, "Clone Remote\u{2026}").key("welcome:clone"),
        spacer(),
    ])
    .gap(tokens::SPACE_2)
    .width(Size::Fill(1.0));

    let mut stack: Vec<El> = vec![hero, actions];
    if !recent.is_empty() {
        stack.push(recent_section(recent));
    }

    // Inner column: fixed 560 wide so hero + actions get a stable
    // frame; `Align::Center` centers any Hug-width child (the recent
    // section) horizontally within that frame, while Fill children
    // (hero, actions) still expand to the full width.
    let content = column(stack)
        .gap(tokens::SPACE_5)
        .align(Align::Center)
        .width(Size::Fixed(CONTENT_COLUMN_WIDTH));

    column([spacer(), content, spacer()])
        .align(Align::Center)
        .height(Size::Fill(1.0))
        .width(Size::Fill(1.0))
        .padding(tokens::SPACE_4)
}

fn recent_section(recent: &[RecentRepoEntry]) -> El {
    let rows = recent
        .iter()
        .enumerate()
        .map(|(idx, path)| recent_row(idx, path));

    // Hug width: the column shrinks to the widest row's intrinsic
    // content (folder icon + name / path stack). The parent content
    // column's `Align::Center` then horizontally centers this section
    // under the hero, instead of having short repo names cling to the
    // left edge of a 560-px-wide frame.
    column([h3("Recent"), item_group(rows).width(Size::Hug)])
        .gap(tokens::SPACE_2)
        .width(Size::Hug)
}

fn recent_row(idx: usize, entry: &RecentRepoEntry) -> El {
    // Two-line content keeps the path tightly associated with its
    // name (no idle row background to bind them otherwise). Hug
    // widths on title + description let the row's intrinsic width
    // be content-driven, so `recent_section`'s outer column can
    // shrink the section to fit the longest row and center the
    // whole list under the hero.
    item([
        item_media_icon(IconName::Folder),
        item_content([
            item_title(entry.name.clone()).width(Size::Hug),
            item_description(entry.description.clone()).width(Size::Hug),
        ])
        .width(Size::Hug),
    ])
    .width(Size::Hug)
    .key(format!("welcome:recent:{idx}"))
}
