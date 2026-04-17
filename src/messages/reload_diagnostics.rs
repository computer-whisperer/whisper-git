use std::collections::HashMap;

use git2::Oid;

use crate::git::CommitInfo;

use super::MessageViewState;

/// Lightweight snapshot of diffable repo state for diagnostic reload comparison.
type SubmoduleSnapshot = (String, Option<bool>, Option<Oid>, Option<Oid>, Option<Oid>);

pub struct RepoStateSnapshot {
    pub commit_oids: Vec<Oid>,
    pub head_oid: Option<Oid>,
    pub current_branch: String,
    pub branch_tips: Vec<(String, Oid, bool)>,
    pub tags: Vec<(String, Oid)>,
    pub stashes: Vec<(usize, String)>,
    pub worktrees: Vec<(String, Option<bool>, Option<usize>)>,
    pub staged_count: usize,
    pub unstaged_count: usize,
    pub untracked_count: usize,
    pub conflicted_count: usize,
    pub ahead_behind: HashMap<String, (usize, usize)>,
    pub submodules: Vec<SubmoduleSnapshot>, // (path, is_dirty, head_pin_oid, index_pin_oid, workdir_oid)
}

impl RepoStateSnapshot {
    /// Capture what the UI currently believes from cached view state.
    pub fn from_ui(
        commits: &[CommitInfo],
        view_state: &MessageViewState<'_>,
        current_branch: &str,
        head_oid: Option<Oid>,
    ) -> Self {
        let commit_oids: Vec<Oid> = commits
            .iter()
            .filter(|c| !c.is_synthetic)
            .map(|c| c.id)
            .collect();

        let branch_tips: Vec<(String, Oid, bool)> = view_state
            .commit_graph_view
            .branch_tips
            .iter()
            .map(|t| (t.name.clone(), t.oid, t.is_remote))
            .collect();

        let tags: Vec<(String, Oid)> = view_state
            .commit_graph_view
            .tags
            .iter()
            .map(|t| (t.name.clone(), t.oid))
            .collect();

        let stashes: Vec<(usize, String)> = view_state
            .branch_sidebar
            .stashes
            .iter()
            .map(|s| (s.index, s.message.clone()))
            .collect();

        let worktrees: Vec<(String, Option<bool>, Option<usize>)> = view_state
            .worktrees
            .iter()
            .map(|w| (w.name.clone(), w.is_dirty, w.dirty_file_count))
            .collect();

        let staged_count = view_state.staging_well.staged_list.files.len();
        let unstaged_count = view_state.staging_well.unstaged_list.files.len();
        let untracked_count = view_state.staging_well.untracked_list.files.len();
        let conflicted_count = view_state.staging_well.conflicted_list.files.len();

        let ahead_behind = view_state.branch_sidebar.ahead_behind_cache();

        let submodules: Vec<SubmoduleSnapshot> = view_state
            .staging_well
            .submodules
            .iter()
            .map(|s| {
                (
                    s.path.clone(),
                    s.is_dirty,
                    s.head_oid,
                    s.index_oid,
                    s.workdir_oid,
                )
            })
            .collect();

        let current_branch = current_branch.to_string();

        Self {
            commit_oids,
            head_oid,
            current_branch,
            branch_tips,
            tags,
            stashes,
            worktrees,
            staged_count,
            unstaged_count,
            untracked_count,
            conflicted_count,
            ahead_behind,
            submodules,
        }
    }
}

/// Compare two snapshots and produce human-readable delta descriptions.
pub fn compute_reload_deltas(before: &RepoStateSnapshot, after: &RepoStateSnapshot) -> Vec<String> {
    let mut deltas = Vec::new();

    // Commits: set diff
    {
        use std::collections::HashSet;
        let before_set: HashSet<&Oid> = before.commit_oids.iter().collect();
        let after_set: HashSet<&Oid> = after.commit_oids.iter().collect();
        let added = after_set.difference(&before_set).count();
        let removed = before_set.difference(&after_set).count();
        if added > 0 || removed > 0 {
            deltas.push(format!("Commits: +{} added, -{} removed", added, removed));
        }
    }

    // HEAD
    if before.head_oid != after.head_oid {
        let fmt_oid = |o: &Option<Oid>| match o {
            Some(oid) => oid.to_string()[..7].to_string(),
            None => "None".to_string(),
        };
        deltas.push(format!(
            "HEAD moved: {} -> {}",
            fmt_oid(&before.head_oid),
            fmt_oid(&after.head_oid)
        ));
    }

    // Current branch
    if before.current_branch != after.current_branch {
        deltas.push(format!(
            "Branch: '{}' -> '{}'",
            before.current_branch, after.current_branch
        ));
    }

    // Branch tips
    {
        let before_map: HashMap<(&str, bool), Oid> = before
            .branch_tips
            .iter()
            .map(|(n, o, r)| ((n.as_str(), *r), *o))
            .collect();
        let after_map: HashMap<(&str, bool), Oid> = after
            .branch_tips
            .iter()
            .map(|(n, o, r)| ((n.as_str(), *r), *o))
            .collect();
        for (key, oid) in &after_map {
            match before_map.get(key) {
                None => deltas.push(format!(
                    "Branch added: {}{}",
                    if key.1 { "(remote) " } else { "" },
                    key.0
                )),
                Some(old_oid) if old_oid != oid => {
                    deltas.push(format!(
                        "Branch moved: {} {} -> {}",
                        key.0,
                        &old_oid.to_string()[..7],
                        &oid.to_string()[..7]
                    ));
                }
                _ => {}
            }
        }
        for key in before_map.keys() {
            if !after_map.contains_key(key) {
                deltas.push(format!(
                    "Branch removed: {}{}",
                    if key.1 { "(remote) " } else { "" },
                    key.0
                ));
            }
        }
    }

    // Tags
    {
        let before_tags: HashMap<&str, Oid> =
            before.tags.iter().map(|(n, o)| (n.as_str(), *o)).collect();
        let after_tags: HashMap<&str, Oid> =
            after.tags.iter().map(|(n, o)| (n.as_str(), *o)).collect();
        for name in after_tags.keys() {
            if !before_tags.contains_key(name) {
                deltas.push(format!("Tag added: {}", name));
            }
        }
        for name in before_tags.keys() {
            if !after_tags.contains_key(name) {
                deltas.push(format!("Tag removed: {}", name));
            }
        }
    }

    // Stashes
    if before.stashes.len() != after.stashes.len() {
        deltas.push(format!(
            "Stashes: {} -> {}",
            before.stashes.len(),
            after.stashes.len()
        ));
    }

    // Worktrees
    for after_wt in &after.worktrees {
        if let Some(before_wt) = before.worktrees.iter().find(|w| w.0 == after_wt.0) {
            if before_wt.1 != after_wt.1 || before_wt.2 != after_wt.2 {
                deltas.push(format!(
                    "Worktree '{}': dirty {:?}({:?}) -> {:?}({:?})",
                    after_wt.0, before_wt.1, before_wt.2, after_wt.1, after_wt.2
                ));
            }
        } else {
            deltas.push(format!("Worktree added: {}", after_wt.0));
        }
    }
    for before_wt in &before.worktrees {
        if !after.worktrees.iter().any(|w| w.0 == before_wt.0) {
            deltas.push(format!("Worktree removed: {}", before_wt.0));
        }
    }

    // Status counts
    if before.staged_count != after.staged_count {
        deltas.push(format!(
            "Staged: {} -> {}",
            before.staged_count, after.staged_count
        ));
    }
    if before.unstaged_count != after.unstaged_count {
        deltas.push(format!(
            "Unstaged: {} -> {}",
            before.unstaged_count, after.unstaged_count
        ));
    }
    if before.untracked_count != after.untracked_count {
        deltas.push(format!(
            "Untracked: {} -> {}",
            before.untracked_count, after.untracked_count
        ));
    }
    if before.conflicted_count != after.conflicted_count {
        deltas.push(format!(
            "Conflicted: {} -> {}",
            before.conflicted_count, after.conflicted_count
        ));
    }

    // Ahead/behind
    {
        let mut all_branches: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for key in before.ahead_behind.keys() {
            all_branches.insert(key.as_str());
        }
        for key in after.ahead_behind.keys() {
            all_branches.insert(key.as_str());
        }
        for branch in all_branches {
            let b = before.ahead_behind.get(branch).copied().unwrap_or((0, 0));
            let a = after.ahead_behind.get(branch).copied().unwrap_or((0, 0));
            if b != a {
                deltas.push(format!(
                    "Ahead/behind '{}': ({},{}) -> ({},{})",
                    branch, b.0, b.1, a.0, a.1
                ));
            }
        }
    }

    // Submodules
    for after_sm in &after.submodules {
        if let Some(before_sm) = before.submodules.iter().find(|s| s.0 == after_sm.0) {
            if before_sm.1 != after_sm.1 {
                deltas.push(format!(
                    "Submodule '{}': dirty {:?} -> {:?}",
                    after_sm.0, before_sm.1, after_sm.1
                ));
            }
            let fmt = |oid: Option<Oid>| {
                oid.map(|o| o.to_string()[..7].to_string())
                    .unwrap_or_else(|| "None".to_string())
            };
            if before_sm.2 != after_sm.2 {
                deltas.push(format!(
                    "Submodule '{}': HEAD pin {} -> {}",
                    after_sm.0,
                    fmt(before_sm.2),
                    fmt(after_sm.2)
                ));
            }
            if before_sm.3 != after_sm.3 {
                deltas.push(format!(
                    "Submodule '{}': index pin {} -> {}",
                    after_sm.0,
                    fmt(before_sm.3),
                    fmt(after_sm.3)
                ));
            }
            if before_sm.4 != after_sm.4 {
                deltas.push(format!(
                    "Submodule '{}': workdir {} -> {}",
                    after_sm.0,
                    fmt(before_sm.4),
                    fmt(after_sm.4)
                ));
            }
        } else {
            deltas.push(format!("Submodule added: {}", after_sm.0));
        }
    }
    for before_sm in &before.submodules {
        if !after.submodules.iter().any(|s| s.0 == before_sm.0) {
            deltas.push(format!("Submodule removed: {}", before_sm.0));
        }
    }

    deltas
}
