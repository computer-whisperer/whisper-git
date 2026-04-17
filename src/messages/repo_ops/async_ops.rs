use crate::git::{self, GitRepo};
use crate::ui::widgets::ToastManager;

use super::super::{AppMessage, MessageViewState, queue_async_op, resolve_cmd_dir};

pub(super) fn handle_repo_async_ops_message(
    msg: &AppMessage,
    repo: &GitRepo,
    staging_repo: &GitRepo,
    view_state: &mut MessageViewState<'_>,
    toast_manager: &mut ToastManager,
) -> Option<bool> {
    let proxy = view_state.proxy.clone();
    match msg {
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
        _ => None,
    }
}
