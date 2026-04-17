use git2::Oid;

use crate::git::{CommitInfo, DiffFile, GitRepo};
use crate::ui::widgets::{ToastManager, ToastSeverity};

use super::super::{AppMessage, MessageViewState, RightPanelMode};

pub(super) fn handle_commit_view_message(
    msg: &AppMessage,
    repo: &GitRepo,
    staging_repo: &GitRepo,
    commits: &mut [CommitInfo],
    view_state: &mut MessageViewState<'_>,
    toast_manager: &mut ToastManager,
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
        _ => None,
    }
}
