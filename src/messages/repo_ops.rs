use crate::git::{self, GitRepo};
use crate::ui::widgets::{ToastManager, ToastSeverity};

use super::{AppMessage, MessageViewState, handle_repo_mutation, queue_async_op, resolve_cmd_dir};

/// Handle repo mutation and async-op messages that are not staging/remote-sync/history.
/// Returns `Some(handled)` when the message belongs to this domain.
pub(super) fn handle_repo_ops_message(
    msg: &AppMessage,
    repo: &GitRepo,
    staging_repo: &GitRepo,
    view_state: &mut MessageViewState<'_>,
    toast_manager: &mut ToastManager,
) -> Option<bool> {
    let proxy = view_state.proxy.clone();
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
        AppMessage::DeleteSubmodule(submodule_path) => {
            let cmd_dir = staging_repo.git_command_dir();
            let rx = git::remove_submodule_async(cmd_dir, submodule_path.clone(), proxy.clone());
            queue_async_op(
                view_state.generic_op_receiver,
                rx,
                format!("Delete submodule '{}'", submodule_path),
                format!("Removing submodule '{}'...", submodule_path),
                toast_manager,
            );
            Some(true)
        }
        AppMessage::UpdateSubmodule(submodule_path) => {
            let cmd_dir = staging_repo.git_command_dir();
            let rx = git::update_submodule_async(cmd_dir, submodule_path.clone(), proxy.clone());
            queue_async_op(
                view_state.generic_op_receiver,
                rx,
                format!("Update submodule '{}'", submodule_path),
                format!("Updating submodule '{}'...", submodule_path),
                toast_manager,
            );
            Some(true)
        }
        AppMessage::ResetSubmodule(submodule_path) => {
            let cmd_dir = staging_repo.git_command_dir();
            let rx = git::reset_submodule_async(cmd_dir, submodule_path.clone(), proxy.clone());
            queue_async_op(
                view_state.generic_op_receiver,
                rx,
                format!("Reset submodule '{}'", submodule_path),
                format!("Resetting submodule '{}' checkout...", submodule_path),
                toast_manager,
            );
            Some(true)
        }
        AppMessage::RemoveWorktree(name) => {
            let cmd_dir = repo.git_command_dir();
            let rx = git::remove_worktree_async(cmd_dir, name.clone(), proxy.clone());
            queue_async_op(
                view_state.generic_op_receiver,
                rx,
                format!("Remove worktree '{}'", name),
                format!("Removing worktree '{}'...", name),
                toast_manager,
            );
            Some(true)
        }
        AppMessage::CreateWorktree(name, source, init_submodules, checkout_lfs) => {
            let cmd_dir = repo.git_command_dir();
            // Compute worktree path: peer directory next to the current workdir
            let wt_path_buf = cmd_dir.parent().unwrap_or(&cmd_dir).join(name);
            let wt_path = wt_path_buf.to_string_lossy().to_string();
            // Heuristic: if source looks like a hex SHA (7+ hex chars), use detached mode
            let is_sha = source.len() >= 7 && source.chars().all(|c| c.is_ascii_hexdigit());
            let rx = if *init_submodules || *checkout_lfs {
                git::create_worktree_with_post_steps_async(
                    cmd_dir,
                    wt_path,
                    source.clone(),
                    is_sha,
                    *init_submodules,
                    *checkout_lfs,
                    proxy.clone(),
                )
            } else if is_sha {
                git::create_worktree_detached_async(cmd_dir, wt_path, source.clone(), proxy.clone())
            } else {
                git::create_worktree_async(cmd_dir, wt_path, source.clone(), proxy.clone())
            };
            queue_async_op(
                view_state.generic_op_receiver,
                rx,
                format!("Create worktree '{}'", name),
                format!("Creating worktree '{}'...", name),
                toast_manager,
            );
            Some(true)
        }
        AppMessage::MergeBranch(name, target_dir) => {
            let cmd_dir = resolve_cmd_dir(target_dir, staging_repo);
            let rx = git::merge_branch_async(cmd_dir, name.clone(), proxy.clone());
            queue_async_op(
                view_state.generic_op_receiver,
                rx,
                format!("Merge '{}'", name),
                format!("Merging '{}'...", name),
                toast_manager,
            );
            Some(true)
        }
        AppMessage::MergeNoFf(name, message, target_dir) => {
            let cmd_dir = resolve_cmd_dir(target_dir, staging_repo);
            let rx = git::merge_noff_async(cmd_dir, name.clone(), message.clone(), proxy.clone());
            queue_async_op(
                view_state.generic_op_receiver,
                rx,
                format!("Merge --no-ff '{}'", name),
                format!("Merging '{}' (no-ff)...", name),
                toast_manager,
            );
            Some(true)
        }
        AppMessage::MergeFfOnly(name, target_dir) => {
            let cmd_dir = resolve_cmd_dir(target_dir, staging_repo);
            let rx = git::merge_ffonly_async(cmd_dir, name.clone(), proxy.clone());
            queue_async_op(
                view_state.generic_op_receiver,
                rx,
                format!("Merge --ff-only '{}'", name),
                format!("Merging '{}' (ff-only)...", name),
                toast_manager,
            );
            Some(true)
        }
        AppMessage::MergeSquash(name, target_dir) => {
            let cmd_dir = resolve_cmd_dir(target_dir, staging_repo);
            let rx = git::merge_squash_async(cmd_dir, name.clone(), proxy.clone());
            queue_async_op(
                view_state.generic_op_receiver,
                rx,
                format!("Squash merge '{}'", name),
                format!("Squash merging '{}'...", name),
                toast_manager,
            );
            Some(true)
        }
        AppMessage::RebaseBranchWithOptions(name, autostash, rebase_merges, target_dir) => {
            let cmd_dir = resolve_cmd_dir(target_dir, staging_repo);
            let rx = git::rebase_with_options_async(
                cmd_dir,
                name.clone(),
                *autostash,
                *rebase_merges,
                proxy.clone(),
            );
            queue_async_op(
                view_state.generic_op_receiver,
                rx,
                format!("Rebase onto '{}'", name),
                format!("Rebasing onto '{}'...", name),
                toast_manager,
            );
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
        AppMessage::StashPush => {
            let cmd_dir = staging_repo.git_command_dir();
            let rx = git::stash_push_async(cmd_dir, proxy.clone());
            queue_async_op(
                view_state.generic_op_receiver,
                rx,
                "Stash push".to_string(),
                "Stashing changes...".to_string(),
                toast_manager,
            );
            Some(true)
        }
        AppMessage::StashPop => {
            let cmd_dir = staging_repo.git_command_dir();
            let rx = git::stash_pop_async(cmd_dir, proxy.clone());
            queue_async_op(
                view_state.generic_op_receiver,
                rx,
                "Stash pop".to_string(),
                "Popping stash...".to_string(),
                toast_manager,
            );
            Some(true)
        }
        AppMessage::StashApply(index) => {
            let cmd_dir = staging_repo.git_command_dir();
            let rx = git::stash_apply_async(cmd_dir, *index, proxy.clone());
            queue_async_op(
                view_state.generic_op_receiver,
                rx,
                format!("Stash apply @{{{}}}", index),
                format!("Applying stash@{{{}}}...", index),
                toast_manager,
            );
            Some(true)
        }
        AppMessage::StashDrop(index) => {
            let cmd_dir = staging_repo.git_command_dir();
            let rx = git::stash_drop_async(cmd_dir, *index, proxy.clone());
            queue_async_op(
                view_state.generic_op_receiver,
                rx,
                format!("Stash drop @{{{}}}", index),
                format!("Dropping stash@{{{}}}...", index),
                toast_manager,
            );
            Some(true)
        }
        AppMessage::StashPopIndex(index) => {
            let cmd_dir = staging_repo.git_command_dir();
            let rx = git::stash_pop_index_async(cmd_dir, *index, proxy.clone());
            queue_async_op(
                view_state.generic_op_receiver,
                rx,
                format!("Stash pop @{{{}}}", index),
                format!("Popping stash@{{{}}}...", index),
                toast_manager,
            );
            Some(true)
        }
        AppMessage::CherryPick(oid, target_dir) => {
            let cmd_dir = resolve_cmd_dir(target_dir, staging_repo);
            let sha = oid.to_string();
            let rx = git::cherry_pick_async(cmd_dir, sha.clone(), proxy.clone());
            queue_async_op(
                view_state.generic_op_receiver,
                rx,
                format!("Cherry-pick {}", &sha[..7]),
                format!("Cherry-picking {}...", &sha[..7]),
                toast_manager,
            );
            Some(true)
        }
        AppMessage::RevertCommit(oid, target_dir) => {
            let cmd_dir = resolve_cmd_dir(target_dir, staging_repo);
            let sha = oid.to_string();
            let rx = git::revert_commit_async(cmd_dir, sha.clone(), proxy.clone());
            queue_async_op(
                view_state.generic_op_receiver,
                rx,
                format!("Revert {}", &sha[..7]),
                format!("Reverting {}...", &sha[..7]),
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
        AppMessage::DeleteRemoteBranch(remote, branch) => {
            let cmd_dir = repo.git_command_dir();
            let rx =
                git::delete_remote_branch_async(cmd_dir, remote.clone(), branch.clone(), proxy);
            queue_async_op(
                view_state.generic_op_receiver,
                rx,
                format!("Delete '{}/{}'", remote, branch),
                format!("Deleting remote branch '{}/{}'...", remote, branch),
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
