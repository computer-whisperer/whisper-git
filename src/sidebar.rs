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

    // Inset the scroll's clip rect by `RING_WIDTH` on the horizontal
    // axis so the per-row focus ring (paint_overflow band) doesn't get
    // clipped at the panel's left/right edge — the row's bbox spans
    // the full inner width, so without padding the ring band is cut
    // by 2px on both sides (`FocusRingObscured` lint).
    sidebar_panel([scroll([body])
        .key("sidebar:scroll")
        .padding(Sides::xy(tokens::RING_WIDTH, 0.0))])
    .padding(0.0)
}

fn section_block(tab: &RepoTab, section: SidebarSection) -> El {
    let collapsed = tab.sidebar.is_collapsed(section);
    let header = section_header(tab, section, collapsed);
    let mut children = vec![header];
    if !collapsed && let Some(body) = section_body(tab, section) {
        children.push(body);
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
    // Local + Tags grow a small "+" affordance to open their create
    // modals. Clicking the + would otherwise toggle the section
    // collapse via the parent tree_row's route — we keep the route on
    // the row but the icon button gets its own key to override.
    //
    // A bare clickable icon — not `icon_button` — because the
    // dense 28 px tree_row can't host a button-shaped affordance:
    // icon_button bakes in a 32 px CONTROL_HEIGHT and surface
    // chrome that pokes above and below the row, visibly colliding
    // with the next entry. The bare icon honours the row's
    // metrics, picks up the tree_row's hover/focus envelope via
    // .focusable(), and reads as an in-row affordance the same
    // way the chevron caret does.
    let create = match section {
        SidebarSection::Local => Some(("new_branch", "Create branch")),
        SidebarSection::Tags => Some(("new_tag", "Create tag")),
        _ => None,
    };
    if let Some((key, tooltip)) = create {
        children.push(
            icon(IconName::Plus)
                .muted()
                .key(key)
                .focusable()
                .cursor(Cursor::Pointer)
                .tooltip(tooltip),
        );
    }

    tree_row(children, format!("section:{}", section.key()))
}

fn section_count(tab: &RepoTab, section: SidebarSection) -> usize {
    match section {
        SidebarSection::Local => tab.local_branches().len(),
        SidebarSection::Remote => remote_names(tab).len(),
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
    let groups: std::collections::BTreeMap<String, Vec<String>> =
        tab.remote_branches().into_iter().collect();
    let remotes = remote_names(tab);
    if remotes.is_empty() {
        return None;
    }
    let mut rows: Vec<El> = Vec::new();
    for remote in remotes {
        let branches = groups.get(&remote).cloned().unwrap_or_default();
        let collapsed = tab.sidebar.is_remote_collapsed(&remote);
        let caret = if collapsed {
            IconName::ChevronRight
        } else {
            IconName::ChevronDown
        };
        // Brand mark inferred from the remote URL — github invertocat
        // for github.com, gitlab tanuki for gitlab.* — so the user can
        // tell at a glance where each remote points. Unknown hosts get
        // no decorator and just show the bare name.
        let provider_icon = tab
            .repo
            .remote_url(&remote)
            .and_then(|url| crate::widgets::brand_icons::for_remote_url(&url));
        // Sub-group header within Remote section. `tree_row` makes
        // the whole row a focusable click target so the caret toggles
        // collapse like the section_header above.
        let mut header: Vec<El> = vec![icon(caret).muted()];
        if let Some(brand) = provider_icon {
            header.push(icon(brand));
        }
        header.extend([
            text(remote.clone()).caption(),
            spacer(),
            badge(branches.len().to_string()).muted(),
        ]);
        rows.push(tree_row(header, format!("remote_group:{}", remote)));
        if collapsed {
            continue;
        }
        if branches.is_empty() {
            rows.push(empty_remote_row());
            continue;
        }
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

fn remote_names(tab: &RepoTab) -> Vec<String> {
    let mut remotes: std::collections::BTreeSet<String> = tab.remotes.iter().cloned().collect();
    remotes.extend(tab.remote_branches().into_iter().map(|(remote, _)| remote));
    remotes.into_iter().collect()
}

fn empty_remote_row() -> El {
    row([
        icon(IconName::GitBranch).muted(),
        text("No fetched branches").caption().muted(),
    ])
    .padding(Sides {
        left: tokens::SPACE_4,
        right: tokens::SPACE_2,
        top: tokens::SPACE_1,
        bottom: tokens::SPACE_1,
    })
    .gap(tokens::SPACE_1)
    .align(Align::Center)
    .height(Size::Fixed(28.0))
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
