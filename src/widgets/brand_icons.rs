//! Custom SVG glyphs not covered by damascene's built-in icon set.
//!
//! Brand marks (Octicons / Simple Icons, CC0) plus the lucide panel
//! toggles (ISC). All parsed once into `SvgIcon`s with
//! `parse_current_color`, so callers tint via `text_color` the same
//! way they do for built-in lucide icons.
//!
//! SVG sources live alongside the binary in `assets/icons/*.svg`.

use std::sync::LazyLock;

use damascene_core::SvgIcon;

const GITHUB_SVG: &str = include_str!("../../assets/icons/github.svg");
const GITLAB_SVG: &str = include_str!("../../assets/icons/gitlab.svg");
const PANEL_LEFT_SVG: &str = include_str!("../../assets/icons/panel-left.svg");
const PANEL_RIGHT_SVG: &str = include_str!("../../assets/icons/panel-right.svg");

pub static GITHUB: LazyLock<SvgIcon> =
    LazyLock::new(|| SvgIcon::parse_current_color(GITHUB_SVG).expect("parse github.svg"));

pub static GITLAB: LazyLock<SvgIcon> =
    LazyLock::new(|| SvgIcon::parse_current_color(GITLAB_SVG).expect("parse gitlab.svg"));

pub static PANEL_LEFT: LazyLock<SvgIcon> =
    LazyLock::new(|| SvgIcon::parse_current_color(PANEL_LEFT_SVG).expect("parse panel-left.svg"));

pub static PANEL_RIGHT: LazyLock<SvgIcon> =
    LazyLock::new(|| SvgIcon::parse_current_color(PANEL_RIGHT_SVG).expect("parse panel-right.svg"));

/// Provider mark for the given [`crate::ci::CiProvider`]. Returns a
/// cheap `Arc`-cloned `SvgIcon` ready to hand to `icon(...)`.
pub fn for_provider(provider: crate::ci::CiProvider) -> SvgIcon {
    match provider {
        crate::ci::CiProvider::GitHub => GITHUB.clone(),
        crate::ci::CiProvider::GitLab => GITLAB.clone(),
    }
}

/// Provider mark inferred from a remote URL. Returns `None` for remotes
/// whose host isn't recognised. Reuses the same URL parsers as the CI
/// dispatch so the icon stays in sync with which providers we know how
/// to talk to.
pub fn for_remote_url(url: &str) -> Option<SvgIcon> {
    if crate::github::parse_github_remote(url).is_some() {
        return Some(GITHUB.clone());
    }
    if crate::gitlab::parse_gitlab_remote(url).is_some() {
        return Some(GITLAB.clone());
    }
    None
}
