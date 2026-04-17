use crate::git::GitRepo;
use crate::ui::widgets::{ToastManager, ToastSeverity};

use super::super::{AppMessage, MessageViewState, handle_repo_mutation};

pub(super) fn handle_repo_mutation_ops_message(
    msg: &AppMessage,
    repo: &GitRepo,
    staging_repo: &GitRepo,
    view_state: &mut MessageViewState<'_>,
    toast_manager: &mut ToastManager,
) -> Option<bool> {
    match msg {
        AppMessage::CheckoutBranch(name) => {
            handle_repo_mutation(
                staging_repo.checkout_branch(name),
                format!("Switched to {}", name),
                "Checkout failed",
                view_state,
                toast_manager,
            );
            Some(true)
        }
        AppMessage::CheckoutRemoteBranch(remote, branch) => {
            handle_repo_mutation(
                staging_repo.checkout_remote_branch(remote, branch),
                format!("Switched to {}/{}", remote, branch),
                "Checkout failed",
                view_state,
                toast_manager,
            );
            Some(true)
        }
        AppMessage::CheckoutCommit(oid, target_dir) => {
            let target_repo = target_dir.as_ref().and_then(|d| GitRepo::open(d).ok());
            let checkout_repo = target_repo.as_ref().unwrap_or(staging_repo);
            handle_repo_mutation(
                checkout_repo.checkout_commit_detached(*oid),
                format!("Checked out {} (detached HEAD)", &oid.to_string()[..7]),
                "Checkout failed",
                view_state,
                toast_manager,
            );
            Some(true)
        }
        AppMessage::DeleteBranch(name) => {
            handle_repo_mutation(
                repo.delete_branch(name),
                format!("Deleted branch {}", name),
                &format!("Cannot delete '{}'", name),
                view_state,
                toast_manager,
            );
            Some(true)
        }
        AppMessage::RenameBranch(old_name, new_name) => {
            handle_repo_mutation(
                repo.rename_branch(old_name, new_name, false),
                format!("Renamed branch '{}' to '{}'", old_name, new_name),
                &format!("Cannot rename '{}'", old_name),
                view_state,
                toast_manager,
            );
            Some(true)
        }
        AppMessage::StageHunk(path, hunk_idx) => {
            match staging_repo.stage_hunk(path, *hunk_idx) {
                Ok(()) => {
                    toast_manager.push(
                        format!("Staged hunk {} in {}", hunk_idx + 1, path),
                        ToastSeverity::Success,
                    );
                    if let Ok(hunks) = staging_repo.diff_working_file(path, false) {
                        if hunks.is_empty() {
                            view_state.diff_view.clear();
                        } else {
                            let diff_file = crate::git::DiffFile::from_hunks(path.clone(), hunks);
                            view_state.diff_view.set_diff(vec![diff_file], path.clone());
                        }
                    }
                }
                Err(e) => {
                    toast_manager.push(format!("Stage hunk failed: {}", e), ToastSeverity::Error);
                }
            }
            Some(true)
        }
        AppMessage::UnstageHunk(path, hunk_idx) => {
            match staging_repo.unstage_hunk(path, *hunk_idx) {
                Ok(()) => {
                    toast_manager.push(
                        format!("Unstaged hunk {} in {}", hunk_idx + 1, path),
                        ToastSeverity::Success,
                    );
                    if let Ok(hunks) = staging_repo.diff_working_file(path, true) {
                        if hunks.is_empty() {
                            view_state.diff_view.clear();
                        } else {
                            let diff_file = crate::git::DiffFile::from_hunks(path.clone(), hunks);
                            view_state
                                .diff_view
                                .set_staged_diff(vec![diff_file], path.clone());
                        }
                    }
                }
                Err(e) => {
                    toast_manager.push(format!("Unstage hunk failed: {}", e), ToastSeverity::Error);
                }
            }
            Some(true)
        }
        AppMessage::DiscardFile(path) => {
            match staging_repo.discard_file(path) {
                Ok(()) => {
                    toast_manager.push(format!("Discarded: {}", path), ToastSeverity::Info);
                }
                Err(e) => {
                    toast_manager.push(format!("Discard failed: {}", e), ToastSeverity::Error);
                }
            }
            Some(true)
        }
        AppMessage::DiscardFiles(paths) => {
            let total = paths.len();
            let mut failed = 0;
            for path in paths {
                if staging_repo.discard_file(path).is_err() {
                    failed += 1;
                }
            }
            if failed == 0 {
                toast_manager.push(format!("Discarded {} files", total), ToastSeverity::Info);
            } else {
                toast_manager.push(
                    format!(
                        "Discarded {}/{} files ({} failed)",
                        total - failed,
                        total,
                        failed
                    ),
                    ToastSeverity::Error,
                );
            }
            Some(true)
        }
        AppMessage::DiscardHunk(path, hunk_idx) => {
            match staging_repo.discard_hunk(path, *hunk_idx) {
                Ok(()) => {
                    toast_manager.push(
                        format!("Discarded hunk {} in {}", hunk_idx + 1, path),
                        ToastSeverity::Info,
                    );
                    // Refresh the diff view with remaining hunks
                    if let Ok(hunks) = staging_repo.diff_working_file(path, false) {
                        if hunks.is_empty() {
                            view_state.diff_view.clear();
                        } else {
                            let diff_file = crate::git::DiffFile::from_hunks(path.clone(), hunks);
                            view_state.diff_view.set_diff(vec![diff_file], path.clone());
                        }
                    }
                }
                Err(e) => {
                    toast_manager.push(format!("Discard hunk failed: {}", e), ToastSeverity::Error);
                }
            }
            Some(true)
        }
        AppMessage::CreateBranch(name, oid) => {
            handle_repo_mutation(
                repo.create_branch_at(name, *oid),
                format!("Created branch '{}'", name),
                "Create branch failed",
                view_state,
                toast_manager,
            );
            Some(true)
        }
        AppMessage::CreateTag(name, oid) => {
            handle_repo_mutation(
                repo.create_tag(name, *oid),
                format!("Created tag '{}'", name),
                "Create tag failed",
                view_state,
                toast_manager,
            );
            Some(true)
        }
        AppMessage::DeleteTag(name) => {
            handle_repo_mutation(
                repo.delete_tag(name),
                format!("Deleted tag '{}'", name),
                "Delete tag failed",
                view_state,
                toast_manager,
            );
            Some(true)
        }
        AppMessage::ResetToCommit(oid, mode, target_dir) => {
            let mode_name = match mode {
                git2::ResetType::Soft => "soft",
                git2::ResetType::Mixed => "mixed",
                git2::ResetType::Hard => "hard",
            };
            // Reset the target worktree (or staging_repo if no explicit target)
            let target_repo = target_dir.as_ref().and_then(|d| GitRepo::open(d).ok());
            let reset_repo = target_repo.as_ref().unwrap_or(staging_repo);
            handle_repo_mutation(
                reset_repo.reset_to_commit(*oid, *mode),
                format!("Reset ({}) to {}", mode_name, &oid.to_string()[..7]),
                "Reset failed",
                view_state,
                toast_manager,
            );
            Some(true)
        }
        AppMessage::AbortOperation => {
            handle_repo_mutation(
                staging_repo.cleanup_state(),
                "Operation aborted".to_string(),
                "Abort failed",
                view_state,
                toast_manager,
            );
            Some(true)
        }
        AppMessage::AddRemote(name, url) => {
            handle_repo_mutation(
                repo.add_remote(name, url),
                format!("Added remote '{}'", name),
                "Failed to add remote",
                view_state,
                toast_manager,
            );
            Some(true)
        }
        AppMessage::DeleteRemote(name) => {
            handle_repo_mutation(
                repo.delete_remote(name),
                format!("Deleted remote '{}'", name),
                "Failed to delete remote",
                view_state,
                toast_manager,
            );
            Some(true)
        }
        AppMessage::RenameRemote(old_name, new_name) => {
            handle_repo_mutation(
                repo.rename_remote(old_name, new_name),
                format!("Renamed remote '{}' to '{}'", old_name, new_name),
                "Failed to rename remote",
                view_state,
                toast_manager,
            );
            Some(true)
        }
        AppMessage::SetRemoteUrl(name, url) => {
            handle_repo_mutation(
                repo.set_remote_url(name, url),
                format!("Updated URL for '{}'", name),
                "Failed to update remote URL",
                view_state,
                toast_manager,
            );
            Some(true)
        }
        AppMessage::CheckoutBranchInWorktree(name, wt_path) => {
            match GitRepo::open(wt_path) {
                Ok(wt_repo) => {
                    handle_repo_mutation(
                        wt_repo.checkout_branch(name),
                        format!("Switched to {} in {}", name, wt_path.display()),
                        "Checkout failed",
                        view_state,
                        toast_manager,
                    );
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Failed to open worktree at {}: {}", wt_path.display(), e),
                        ToastSeverity::Error,
                    );
                }
            }
            Some(true)
        }
        AppMessage::SetHead(name) => {
            handle_repo_mutation(
                repo.set_head_to(name),
                format!("HEAD now points to {}", name),
                "Set HEAD failed",
                view_state,
                toast_manager,
            );
            Some(true)
        }
        _ => None,
    }
}
