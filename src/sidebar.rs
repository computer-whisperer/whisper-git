//! Branch sidebar composition.
//!
//! Renders the six collapsible sections (Local / Remote / Tags /
//! Submodules / Worktrees / Stashes) as plain aetna primitives.
//! Toggle keys: `section:<KEY>`. Item keys: `branch:<name>`,
//! `remote:<remote>/<branch>`, `tag:<name>`, `submodule:<name>`,
//! `worktree:<name>`, `stash:<idx>`.

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

    tree_row(
        [
            icon(caret).muted(),
            text(section.label()).caption().muted(),
            spacer(),
            badge(count.to_string()).muted(),
        ],
        format!("section:{}", section.key()),
    )
}

fn section_count(tab: &RepoTab, section: SidebarSection) -> usize {
    match section {
        SidebarSection::Local => tab.local_branches().len(),
        SidebarSection::Remote => tab.remote_branches().iter().map(|(_, b)| b.len()).sum(),
        SidebarSection::Tags => tab.tags.len(),
        SidebarSection::Submodules => tab.submodules.len(),
        SidebarSection::Worktrees => tab.worktrees.len(),
        SidebarSection::Stashes => tab.stashes.len(),
    }
}

fn section_body(tab: &RepoTab, section: SidebarSection) -> Option<El> {
    match section {
        SidebarSection::Local => Some(local_body(tab)),
        SidebarSection::Remote => remote_body(tab),
        SidebarSection::Tags => tags_body(tab),
        SidebarSection::Submodules => submodules_body(tab),
        SidebarSection::Worktrees => worktrees_body(tab),
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

fn submodules_body(tab: &RepoTab) -> Option<El> {
    if tab.submodules.is_empty() {
        return None;
    }
    let rows: Vec<El> = tab
        .submodules
        .iter()
        .map(|s| {
            item_row(
                IconName::Folder,
                &s.name,
                None,
                false,
                false,
                format!("submodule:{}", s.name),
            )
        })
        .collect();
    Some(column(rows))
}

fn worktrees_body(tab: &RepoTab) -> Option<El> {
    if tab.worktrees.is_empty() {
        return None;
    }
    // Compare by Path components rather than raw strings so trailing
    // slashes (libgit2 sometimes adds them, `repo.workdir()` sometimes
    // doesn't) don't desync the highlight from the actual selection.
    let active_components: Option<Vec<_>> = tab
        .active_worktree
        .as_ref()
        .map(|p| p.components().collect());
    let rows: Vec<El> = tab
        .worktrees
        .iter()
        .map(|w| {
            let detail = if w.branch.is_empty() {
                "(detached)".to_string()
            } else {
                w.branch.clone()
            };
            let w_components: Vec<_> =
                std::path::Path::new(&w.path).components().collect();
            let is_active = active_components.as_ref() == Some(&w_components);
            item_row(
                IconName::LayoutDashboard,
                &w.name,
                Some(detail),
                is_active,
                false,
                format!("worktree:{}", w.name),
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
