//! Branch sidebar composition.
//!
//! Renders the four collapsible sections (Local / Remote / Tags /
//! Stashes) as plain aetna primitives. Toggle keys: `section:<KEY>`.
//! Item keys: `branch:<name>`, `remote:<remote>/<branch>`,
//! `tag:<name>`, `stash:<idx>`.
//!
//! Worktrees and submodules deliberately don't live here — see
//! `staging::worktree_selector` for the worktree pill bar (top of
//! the staging well) and the staging-well / commit-detail submodule
//! lists for submodules. Both are properties of the active
//! worktree, not the repo.

use aetna_core::{El, IconName, prelude::*, widgets::sidebar::sidebar as sidebar_panel};

use crate::repo_tab::{RepoTab, SidebarSection, SidebarSelection};

pub fn sidebar(tab: &RepoTab) -> El {
    let sections: Vec<El> = SidebarSection::ALL
        .iter()
        .copied()
        .map(|s| section_block(tab, s))
        .collect();

    let body = column(sections).width(Size::Fill(1.0));

    sidebar_panel([scroll([body]).key("sidebar:scroll")]).padding(0.0)
}

fn section_block(tab: &RepoTab, section: SidebarSection) -> El {
    let collapsed = tab.sidebar.is_collapsed(section);
    let header = section_header(tab, section, collapsed);
    let mut children = vec![header];
    if !collapsed {
        if let Some(body) = section_body(tab, section) {
            children.push(body);
        }
    }
    column(children)
}

fn section_header(tab: &RepoTab, section: SidebarSection, collapsed: bool) -> El {
    let caret = if collapsed {
        IconName::ChevronRight
    } else {
        IconName::ChevronDown
    };
    let count = section_count(tab, section);

    let mut children: Vec<El> = vec![
        icon(caret).muted(),
        text(section.label()).caption().muted(),
        spacer(),
        badge(count.to_string()).muted(),
    ];
    // Local section grows a small "+" affordance to open the create-
    // branch modal. Clicking the + would otherwise toggle the section
    // collapse via the parent tree_row's route — we keep the route on
    // the row but the icon button gets its own key to override.
    if matches!(section, SidebarSection::Local) {
        children.push(
            icon_button(IconName::Plus)
                .key("new_branch")
                .tooltip("Create branch"),
        );
    }

    tree_row(children, format!("section:{}", section.key()))
}

fn section_count(tab: &RepoTab, section: SidebarSection) -> usize {
    match section {
        SidebarSection::Local => tab.local_branches().len(),
        SidebarSection::Remote => tab.remote_branches().iter().map(|(_, b)| b.len()).sum(),
        SidebarSection::Tags => tab.tags.len(),
        SidebarSection::Stashes => tab.stashes.len(),
    }
}

fn section_body(tab: &RepoTab, section: SidebarSection) -> Option<El> {
    match section {
        SidebarSection::Local => Some(local_body(tab)),
        SidebarSection::Remote => remote_body(tab),
        SidebarSection::Tags => tags_body(tab),
        SidebarSection::Stashes => stashes_body(tab),
    }
}

fn local_body(tab: &RepoTab) -> El {
    let current = tab.current_branch();
    let selected = match &tab.sidebar.selected {
        Some(SidebarSelection::Local(n)) => Some(n.as_str()),
        _ => None,
    };
    let rows: Vec<El> = tab
        .local_branches()
        .into_iter()
        .map(|name| {
            let is_head = name == current;
            let is_selected = selected == Some(name);
            // Active branch uses Check (currently checked out) and a
            // primary tint on the label so it's readable at a glance.
            let leading = if is_head {
                IconName::Check
            } else {
                IconName::GitBranch
            };
            item_row(
                leading,
                name,
                None,
                is_head,
                is_selected,
                format!("branch:{}", name),
            )
        })
        .collect();
    column(rows)
}

fn remote_body(tab: &RepoTab) -> Option<El> {
    let groups = tab.remote_branches();
    if groups.is_empty() {
        return None;
    }
    let mut rows: Vec<El> = Vec::new();
    for (remote, branches) in groups {
        // Sub-group header within Remote section.
        rows.push(
            row([
                icon(IconName::ChevronDown),
                text(remote.clone()).caption(),
                spacer(),
                badge(branches.len().to_string()).muted(),
            ])
            .padding(Sides::xy(tokens::SPACE_2, tokens::SPACE_1))
            .gap(tokens::SPACE_1)
            .align(Align::Center),
        );
        for branch in branches {
            let key = format!("remote:{}/{}", remote, branch);
            rows.push(
                item_row(IconName::GitBranch, &branch, None, false, false, key).padding(Sides {
                    left: tokens::SPACE_4,
                    right: tokens::SPACE_2,
                    top: tokens::SPACE_1,
                    bottom: tokens::SPACE_1,
                }),
            );
        }
    }
    Some(column(rows))
}

fn tags_body(tab: &RepoTab) -> Option<El> {
    if tab.tags.is_empty() {
        return None;
    }
    let rows: Vec<El> = tab
        .tags
        .iter()
        .map(|t| {
            item_row(
                IconName::FileText,
                &t.name,
                None,
                false,
                false,
                format!("tag:{}", t.name),
            )
        })
        .collect();
    Some(column(rows))
}

fn stashes_body(tab: &RepoTab) -> Option<El> {
    if tab.stashes.is_empty() {
        return None;
    }
    let rows: Vec<El> = tab
        .stashes
        .iter()
        .enumerate()
        .map(|(idx, s)| {
            item_row(
                IconName::Activity,
                &s.message,
                None,
                false,
                false,
                format!("stash:{}", idx),
            )
        })
        .collect();
    Some(column(rows))
}

/// The dense `tree_row` recipe the README catalog calls out for sidebar
/// trees and resource lists. Bakes the envelope (focusable + cursor +
/// list-item metrics + radius for the focus ring + paint_overflow so
/// the ring has somewhere to render + spring animation) so hover,
/// press, focus-visible, and the `.current()` / `.selected()`
/// chainables all light up like they would on the catalog `item`
/// widget — just at a denser 28 px height. Children are pre-built so
/// each section can stack its own chevron / caption / icon / badge /
/// detail anatomy.
fn tree_row<I, E>(children: I, key: impl Into<String>) -> El
where
    I: IntoIterator<Item = E>,
    E: Into<El>,
{
    row(children)
        .key(key)
        .focusable()
        .style_profile(StyleProfile::Surface)
        .metrics_role(MetricsRole::ListItem)
        .cursor(Cursor::Pointer)
        .paint_overflow(Sides::all(tokens::RING_WIDTH))
        .radius(tokens::RADIUS_SM)
        .animate(Timing::SPRING_QUICK)
        .padding(Sides::xy(tokens::SPACE_2, tokens::SPACE_1))
        .gap(tokens::SPACE_1)
        .align(Align::Center)
        .height(Size::Fixed(28.0))
}

fn item_row(
    leading: IconName,
    label: &str,
    detail: Option<String>,
    is_head: bool,
    is_selected: bool,
    key: String,
) -> El {
    let leading_icon = if is_head {
        icon(leading).text_color(tokens::PRIMARY)
    } else {
        icon(leading).muted()
    };
    let label_el = if is_head {
        text(label.to_string()).text_color(tokens::PRIMARY)
    } else {
        text(label.to_string())
    };
    let mut content: Vec<El> = vec![leading_icon, label_el];
    if let Some(d) = detail {
        content.push(spacer());
        content.push(text(d).caption().muted());
    }
    let row_el = tree_row(content, key);

    match (is_head, is_selected) {
        (true, _) => row_el.current(),
        (_, true) => row_el.selected(),
        _ => row_el,
    }
}
