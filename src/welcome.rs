//! Welcome view — shown when no repo tab is open.
//!
//! Centered hero (logo + title + tagline), then a Open / Clone action
//! row, and a recent-repos column when `Config::recent_repos` is
//! non-empty. Click targets emit:
//!
//! - `open_repo` — reuses the existing open-folder picker
//! - `welcome:clone` — opens the clone modal
//! - `welcome:recent:{idx}` — opens the persisted recent path at that index

use std::path::Path;
use std::sync::LazyLock;

use aetna_core::{El, IconName, SvgIcon, prelude::*};

const HERO_ICON_PX: f32 = 96.0;
const RECENT_COLUMN_WIDTH: f32 = 560.0;

/// App logo, parsed once on first paint. The asset is multi-tone, so
/// we use `parse` (preserves the SVG's own colors) rather than
/// `parse_current_color`.
static LOGO: LazyLock<SvgIcon> = LazyLock::new(|| {
    SvgIcon::parse(include_str!("../assets/git-client-icon.svg"))
        .expect("git-client-icon.svg failed to parse")
});

pub fn welcome_view(recent: &[String]) -> El {
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

    // Inner column: fixed 560 wide, no align — Fill children expand to
    // the full width. Outer column centers it horizontally via the
    // surrounding spacers + align(Center).
    let content = column(stack)
        .gap(tokens::SPACE_5)
        .width(Size::Fixed(RECENT_COLUMN_WIDTH));

    column([spacer(), content, spacer()])
        .align(Align::Center)
        .height(Size::Fill(1.0))
        .width(Size::Fill(1.0))
        .padding(tokens::SPACE_4)
}

fn recent_section(recent: &[String]) -> El {
    let rows = recent
        .iter()
        .enumerate()
        .map(|(idx, path)| recent_row(idx, path));

    column([
        row([h3("Recent"), spacer()])
            .align(Align::Center)
            .width(Size::Fill(1.0)),
        item_group(rows),
    ])
    .gap(tokens::SPACE_2)
    .width(Size::Fill(1.0))
}

fn recent_row(idx: usize, path_str: &str) -> El {
    let path = Path::new(path_str);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path_str.to_string());
    let parent = path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    item([
        item_media_icon(IconName::Folder),
        item_content([item_title(name), item_description(parent)]),
    ])
    .key(format!("welcome:recent:{idx}"))
}
