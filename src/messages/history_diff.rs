use std::collections::HashMap;

use git2::Oid;

use crate::git::{self, CommitInfo, DiffFile, GitRepo};
use crate::ui::widgets::{ToastManager, ToastSeverity};

use super::{AppMessage, MessageContext, MessageViewState, RightPanelMode};

/// Handle commit selection, diff viewing, and history navigation messages.
/// Returns `Some(handled)` when the message belongs to this domain.
pub(super) fn handle_history_diff_message(
    msg: &AppMessage,
    repo: &GitRepo,
    staging_repo: &GitRepo,
    commits: &mut Vec<CommitInfo>,
    view_state: &mut MessageViewState<'_>,
    toast_manager: &mut ToastManager,
    ctx: &MessageContext,
) -> Option<bool> {
    match msg {
        AppMessage::SelectedCommit(oid) => {
            let oid = *oid;
            let full_info = repo.full_commit_info(oid);
            let submodule_entries = repo.submodules_at_commit(oid).unwrap_or_default();
            let ci_summary = view_state
                .commit_graph_view
                .ci_commit_rollups
                .get(&oid.to_string())
                .map(|rollups| {
                    rollups
                        .iter()
                        .map(|r| {
                            format!(
                                "{} {}F {}P {}S",
                                r.provider.short_label(),
                                r.rollup.counts.failure,
                                r.rollup.counts.pending,
                                r.rollup.counts.success
                            )
                        })
                        .collect::<Vec<String>>()
                        .join("  ")
                });
            match repo.diff_for_commit(oid) {
                Ok(diff_files) => {
                    if let Ok(info) = full_info {
                        view_state.commit_detail_view.set_commit(
                            info,
                            diff_files.clone(),
                            submodule_entries,
                            ci_summary,
                        );
                    }
                    if let Some(first_file) = diff_files.first() {
                        let title = first_file.path.clone();
                        view_state
                            .diff_view
                            .set_diff(vec![first_file.clone()], title);
                    } else {
                        let title = commits
                            .iter()
                            .find(|c| c.id == oid)
                            .map(|c| format!("{} {}", c.short_id, c.summary))
                            .unwrap_or_else(|| oid.to_string());
                        view_state.diff_view.set_diff(diff_files, title);
                    }
                    *view_state.last_diff_commit = Some(oid);
                    *view_state.right_panel_mode = RightPanelMode::Browse;
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Failed to load diff for {}: {}", &oid.to_string()[..7], e),
                        ToastSeverity::Error,
                    );
                }
            }
            Some(true)
        }
        AppMessage::ViewCommitFileDiff(oid, path) => {
            match repo.diff_file_in_commit(*oid, path) {
                Ok(diff_files) => {
                    view_state.diff_view.set_diff(diff_files, path.clone());
                }
                Err(e) => {
                    view_state.diff_view.clear();
                    toast_manager.push(
                        format!("Failed to load diff for '{}': {}", path, e),
                        ToastSeverity::Error,
                    );
                }
            }
            Some(true)
        }
        AppMessage::ViewDiff(path, staged) => {
            match staging_repo.diff_working_file(path, *staged) {
                Ok(hunks) => {
                    let diff_file = DiffFile::from_hunks(path.clone(), hunks);
                    let submodule = view_state
                        .staging_well
                        .submodules
                        .iter()
                        .find(|sm| sm.path == *path);
                    let short = |oid: Option<Oid>| match oid {
                        Some(oid) => oid.to_string()[..7].to_string(),
                        None => "-".to_string(),
                    };
                    let title = if let Some(sm) = submodule {
                        if *staged {
                            format!(
                                "Staged Submodule: {} (HEAD {} -> INDEX {})",
                                path,
                                short(sm.head_oid),
                                short(sm.index_oid)
                            )
                        } else {
                            let dirty_suffix = if sm.is_dirty == Some(true) {
                                ", dirty"
                            } else {
                                ""
                            };
                            format!(
                                "Unstaged Submodule: {} (INDEX {} -> WORKDIR {}{})",
                                path,
                                short(sm.index_oid),
                                short(sm.workdir_oid),
                                dirty_suffix
                            )
                        }
                    } else if *staged {
                        format!("Staged: {}", path)
                    } else {
                        format!("Unstaged: {}", path)
                    };
                    if *staged {
                        view_state.diff_view.set_staged_diff(vec![diff_file], title);
                    } else {
                        view_state.diff_view.set_diff(vec![diff_file], title);
                    }
                    view_state
                        .diff_view
                        .set_hunk_actions_enabled(submodule.is_none());
                    *view_state.last_diff_commit = None;
                }
                Err(e) => {
                    view_state.diff_view.clear();
                    toast_manager.push(
                        format!("Failed to load diff for '{}': {}", path, e),
                        ToastSeverity::Error,
                    );
                }
            }
            Some(true)
        }
        AppMessage::LoadMoreCommits => {
            // Count only graph commits (exclude synthetics + orphans) for the load-more request
            let real_count = commits
                .iter()
                .filter(|c| !c.is_synthetic && !c.is_orphaned)
                .count();
            let new_count = real_count + 50;
            // Preserve existing diff stats so they don't flicker away
            let prev_stats: HashMap<Oid, (usize, usize)> = commits
                .iter()
                .filter(|c| c.insertions > 0 || c.deletions > 0)
                .map(|c| (c.id, (c.insertions, c.deletions)))
                .collect();
            let graph_result = if ctx.show_orphaned_commits {
                repo.commit_graph_with_orphans(new_count)
            } else {
                repo.commit_graph(new_count)
            };
            match graph_result {
                Ok(new_commits) => {
                    *commits = new_commits;
                    // Restore cached diff stats until async task provides fresh values
                    for commit in commits.iter_mut() {
                        if let Some(&(ins, del)) = prev_stats.get(&commit.id) {
                            commit.insertions = ins;
                            commit.deletions = del;
                        }
                    }
                    // Re-add synthetic entries sorted by time
                    let worktrees = repo.worktrees().unwrap_or_default();
                    let synthetics = git::create_synthetic_entries(repo, &worktrees, commits);
                    if !synthetics.is_empty() {
                        git::insert_synthetics_sorted(commits, synthetics);
                    }
                    view_state.commit_graph_view.update_layout(commits);
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Failed to load more commits: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
            view_state.commit_graph_view.finish_loading();
            Some(true)
        }
        AppMessage::JumpToWorktreeBranch(name) => {
            // Find the worktree by name, get its branch, find the branch tip, select it
            if let Some(wt) = view_state.worktrees.iter().find(|w| w.name == *name) {
                let branch_name = wt.branch.clone();
                if let Some(tip) = view_state
                    .commit_graph_view
                    .branch_tips
                    .iter()
                    .find(|t| t.name == branch_name && !t.is_remote)
                {
                    view_state.commit_graph_view.selected_commit = Some(tip.oid);
                    view_state
                        .commit_graph_view
                        .scroll_to_selection(commits, ctx.graph_bounds);
                    toast_manager.push(
                        format!("Jumped to branch '{}'", branch_name),
                        ToastSeverity::Info,
                    );
                } else {
                    toast_manager.push(
                        format!("Branch '{}' not found in graph", branch_name),
                        ToastSeverity::Error,
                    );
                }
            } else {
                toast_manager.push(
                    format!("Worktree '{}' not found", name),
                    ToastSeverity::Error,
                );
            }
            Some(true)
        }
        AppMessage::JumpToCommit(oid) => {
            let oid = *oid;
            // Check if the commit is already in the loaded set
            if commits.iter().any(|c| c.id == oid) {
                view_state.commit_graph_view.selected_commit = Some(oid);
                view_state
                    .commit_graph_view
                    .scroll_to_selection(commits, ctx.graph_bounds);
            } else {
                // Find how far back this commit is in the topological walk
                const MAX_SEARCH: usize = 50_000;
                match repo.commit_position_in_walk(oid, MAX_SEARCH) {
                    Ok(Some(position)) => {
                        let needed = position + 10; // small padding
                        // Preserve existing diff stats
                        let prev_stats: HashMap<Oid, (usize, usize)> = commits
                            .iter()
                            .filter(|c| c.insertions > 0 || c.deletions > 0)
                            .map(|c| (c.id, (c.insertions, c.deletions)))
                            .collect();
                        let graph_result = if ctx.show_orphaned_commits {
                            repo.commit_graph_with_orphans(needed)
                        } else {
                            repo.commit_graph(needed)
                        };
                        match graph_result {
                            Ok(new_commits) => {
                                *commits = new_commits;
                                for commit in commits.iter_mut() {
                                    if let Some(&(ins, del)) = prev_stats.get(&commit.id) {
                                        commit.insertions = ins;
                                        commit.deletions = del;
                                    }
                                }
                                let worktrees = repo.worktrees().unwrap_or_default();
                                let synthetics =
                                    git::create_synthetic_entries(repo, &worktrees, commits);
                                if !synthetics.is_empty() {
                                    git::insert_synthetics_sorted(commits, synthetics);
                                }
                                view_state.commit_graph_view.update_layout(commits);
                                view_state.commit_graph_view.selected_commit = Some(oid);
                                view_state
                                    .commit_graph_view
                                    .scroll_to_selection(commits, ctx.graph_bounds);
                            }
                            Err(e) => {
                                toast_manager.push(
                                    format!("Failed to load commits: {}", e),
                                    ToastSeverity::Error,
                                );
                            }
                        }
                    }
                    Ok(None) => {
                        toast_manager.push(
                            "Commit is too far back in history to jump to",
                            ToastSeverity::Error,
                        );
                    }
                    Err(e) => {
                        toast_manager.push(
                            format!("Failed to find commit: {}", e),
                            ToastSeverity::Error,
                        );
                    }
                }
            }
            Some(true)
        }
        _ => None,
    }
}
