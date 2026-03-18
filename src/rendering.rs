//! Rendering pipeline: frame drawing, UI output construction, context menu dispatch,
//! panel chrome, screenshots, and Vulkan helpers.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::Instant;
use vulkano::{
    Validated, VulkanError,
    command_buffer::{
        AutoCommandBufferBuilder, CommandBufferUsage, PrimaryAutoCommandBuffer, RenderPassBeginInfo,
    },
    format::{Format, NumericFormat},
    pipeline::graphics::viewport::Viewport,
    swapchain::{SwapchainPresentInfo, acquire_next_image},
    sync::{self, GpuFuture},
};

use crate::messages::{AppMessage, RightPanelMode};
use crate::renderer::{OffscreenTarget, capture_to_buffer};
use crate::ui::widget::theme;
use crate::ui::widgets::{
    BranchNameDialog, CloneDialog, ConfirmDialog, ErrorDialog, MenuItem, MergeDialog, PullDialog,
    PushDialog, RebaseDialog, RemoteDialog, RepoDialog, SettingsDialog, ShortcutContext, TabBar,
    ToastManager, ToastSeverity, TokenDialog, Tooltip,
};
use crate::ui::{
    AvatarCache, AvatarRenderer, IconRenderer, Rect, ScreenLayout, TextRenderer, Widget,
    WidgetOutput,
};

use crate::submodule_nav::open_terminal_at;

use super::{ActiveModal, App, FocusedPanel, RenderState, RepoTab, TabViewState};

/// Render the preview/diff panel header bar (SURFACE_RAISED background + bold title).
/// Returns the body rect below the header.
pub(crate) fn render_preview_header(
    output: &mut WidgetOutput,
    rect: Rect,
    title: &str,
    is_placeholder: bool,
    scale: f32,
    bold_text_renderer: &TextRenderer,
) -> Rect {
    let header_h = 28.0 * scale;
    let (header_rect, body_rect) = rect.take_top(header_h);
    output
        .spline_vertices
        .extend(crate::ui::widget::create_rect_vertices(
            &header_rect,
            theme::SURFACE_RAISED.to_array(),
        ));
    let header_text_y = header_rect.y + (header_h - bold_text_renderer.line_height()) / 2.0;
    let header_text_x = header_rect.x + 12.0 * scale;
    let color = if is_placeholder {
        theme::TEXT_MUTED
    } else {
        theme::TEXT_BRIGHT
    };
    output
        .bold_text_vertices
        .extend(bold_text_renderer.layout_text(
            title,
            header_text_x,
            header_text_y,
            color.to_array(),
        ));
    body_rect
}

/// Handle a context menu action by dispatching to the appropriate AppMessage
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_context_menu_action(
    action_id: &str,
    view_state: &mut TabViewState,
    toast_manager: &mut ToastManager,
    confirm_dialog: &mut ConfirmDialog,
    branch_name_dialog: &mut BranchNameDialog,
    remote_dialog: &mut RemoteDialog,
    merge_dialog: &mut MergeDialog,
    rebase_dialog: &mut RebaseDialog,
    repo: &crate::git::GitRepo,
    pending_confirm_action: &mut Option<AppMessage>,
    active_modal: &mut Option<ActiveModal>,
) {
    // Actions may be in format "action:param" or just "action"
    let (action, param) = action_id.split_once(':').unwrap_or((action_id, ""));
    let operation_scope = if let Some(focus) = &view_state.submodule_focus {
        format!("submodule '{}'", focus.current_name)
    } else if let Some(path) = view_state.worktree_state.selected_path.as_ref() {
        format!("worktree '{}'", path.display())
    } else {
        "current repository".to_string()
    };

    match action {
        // Commit graph actions
        "copy_sha" => {
            if let Some(oid) = view_state.context_menu_commit {
                let sha = oid.to_string();
                match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(&sha)) {
                    Ok(()) => {
                        toast_manager
                            .push(format!("Copied: {}", &sha[..7]), ToastSeverity::Success);
                    }
                    Err(e) => {
                        toast_manager.push(format!("Clipboard error: {e}"), ToastSeverity::Error);
                    }
                }
            }
        }
        "view_details" => {
            if let Some(oid) = view_state.context_menu_commit {
                view_state
                    .pending_messages
                    .push(AppMessage::SelectedCommit(oid));
            }
        }
        "checkout" => {
            if param.is_empty() {
                // Commit graph checkout: find the branch at the selected commit
                if let Some(oid) = view_state.context_menu_commit
                    && let Some(tip) = view_state
                        .commit_graph_view
                        .branch_tips
                        .iter()
                        .find(|t| t.oid == oid && !t.is_remote)
                {
                    view_state
                        .pending_messages
                        .push(AppMessage::CheckoutBranch(tip.name.clone()));
                }
            } else {
                // Branch sidebar checkout
                view_state
                    .pending_messages
                    .push(AppMessage::CheckoutBranch(param.to_string()));
            }
        }
        "checkout_commit" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                let target_dir = view_state.worktree_state.selected_path.clone();
                confirm_dialog.show(
                    "Checkout Commit (Detached)",
                    &format!(
                        "Checkout commit {} as detached HEAD in {}?",
                        short, operation_scope
                    ),
                );
                *active_modal = Some(ActiveModal::Confirm);
                *pending_confirm_action = Some(AppMessage::CheckoutCommit(oid, target_dir));
            }
        }
        "checkout_remote" => {
            if let Some((remote, branch)) = param.split_once('/') {
                view_state
                    .pending_messages
                    .push(AppMessage::CheckoutRemoteBranch(
                        remote.to_string(),
                        branch.to_string(),
                    ));
            }
        }
        "rename" => {
            if !param.is_empty() {
                branch_name_dialog.show_for_rename(param);
                *active_modal = Some(ActiveModal::BranchName);
            }
        }
        "delete" => {
            if !param.is_empty() {
                confirm_dialog.show(
                    "Delete Branch",
                    &format!("Delete local branch '{}' in {}?", param, operation_scope),
                );
                *active_modal = Some(ActiveModal::Confirm);
                *pending_confirm_action = Some(AppMessage::DeleteBranch(param.to_string()));
            }
        }
        "push" => {
            let branch = if param.is_empty() {
                view_state
                    .current_branch_opt()
                    .unwrap_or("HEAD")
                    .to_string()
            } else {
                param.to_string()
            };
            view_state.pending_messages.push(AppMessage::Push {
                remote: None,
                branch,
            });
        }
        "push_to" => {
            view_state
                .pending_messages
                .push(AppMessage::ShowPushDialog(param.to_string()));
        }
        "pull" => {
            let branch = if param.is_empty() {
                view_state
                    .current_branch_opt()
                    .unwrap_or("HEAD")
                    .to_string()
            } else {
                param.to_string()
            };
            view_state.pending_messages.push(AppMessage::Pull {
                remote: None,
                branch,
            });
        }
        "pull_rebase" => {
            let branch = if param.is_empty() {
                view_state
                    .current_branch_opt()
                    .unwrap_or("HEAD")
                    .to_string()
            } else {
                param.to_string()
            };
            view_state.pending_messages.push(AppMessage::PullRebase {
                remote: None,
                branch,
            });
        }
        "pull_from_dialog" => {
            view_state
                .pending_messages
                .push(AppMessage::ShowPullDialog(param.to_string()));
        }
        "force_push" => {
            let branch = if param.is_empty() {
                view_state
                    .current_branch_opt()
                    .unwrap_or("HEAD")
                    .to_string()
            } else {
                param.to_string()
            };
            confirm_dialog.show(
                "Force Push",
                &format!(
                    "Force push '{}' with --force-with-lease? This may overwrite remote commits.",
                    branch
                ),
            );
            *active_modal = Some(ActiveModal::Confirm);
            *pending_confirm_action = Some(AppMessage::PushForce {
                remote: None,
                branch,
            });
        }
        "fetch_remote" => {
            if !param.is_empty() {
                view_state
                    .pending_messages
                    .push(AppMessage::Fetch(Some(param.to_string())));
            }
        }
        // Staging actions
        "stage" => {
            if !param.is_empty() {
                view_state
                    .pending_messages
                    .push(AppMessage::StageFile(param.to_string()));
            }
        }
        "unstage" => {
            if !param.is_empty() {
                view_state
                    .pending_messages
                    .push(AppMessage::UnstageFile(param.to_string()));
            }
        }
        "view_diff" => {
            if !param.is_empty() {
                let staged = view_state
                    .staging_well
                    .staged_list
                    .files
                    .iter()
                    .any(|f| f.path == param);
                view_state
                    .pending_messages
                    .push(AppMessage::ViewDiff(param.to_string(), staged));
            }
        }
        "discard" => {
            if !param.is_empty() {
                confirm_dialog.show(
                    "Discard Changes",
                    &format!(
                        "Discard changes to '{}' in {}? This cannot be undone.",
                        param, operation_scope
                    ),
                );
                *active_modal = Some(ActiveModal::Confirm);
                *pending_confirm_action = Some(AppMessage::DiscardFile(param.to_string()));
            }
        }
        "delete_submodule" => {
            if !param.is_empty() {
                let sm_label = view_state
                    .staging_well
                    .submodules
                    .iter()
                    .find(|s| s.path == param || s.name == param)
                    .map(|sm| {
                        if sm.name != sm.path {
                            format!("{} ({})", sm.name, sm.path)
                        } else {
                            sm.name.clone()
                        }
                    })
                    .unwrap_or_else(|| param.to_string());
                confirm_dialog.show(
                    "Delete Submodule",
                    &format!(
                        "Remove submodule '{}' in {}? This will deinit and remove it.",
                        sm_label, operation_scope
                    ),
                );
                *active_modal = Some(ActiveModal::Confirm);
                *pending_confirm_action = Some(AppMessage::DeleteSubmodule(param.to_string()));
            }
        }
        "update_submodule" => {
            if !param.is_empty() {
                view_state
                    .pending_messages
                    .push(AppMessage::UpdateSubmodule(param.to_string()));
            }
        }
        "enter_submodule" => {
            if !param.is_empty() {
                view_state
                    .pending_messages
                    .push(AppMessage::EnterSubmodule(param.to_string()));
            }
        }
        "open_submodule" => {
            if !param.is_empty() {
                if let Some(sm) = view_state
                    .staging_well
                    .submodules
                    .iter()
                    .find(|s| s.path == param || s.name == param)
                {
                    let rel_path = sm.path.clone();
                    let label = sm.name.clone();
                    let selected_worktree_dir = || {
                        view_state
                            .worktree_state
                            .staging_repo()
                            .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                            .or_else(|| view_state.worktree_state.selected_path.clone())
                    };
                    let current_repo_dir = || repo.workdir().map(|p| p.to_path_buf());
                    let base_dir = if view_state.submodule_focus.is_some() {
                        current_repo_dir().or_else(selected_worktree_dir)
                    } else {
                        selected_worktree_dir().or_else(current_repo_dir)
                    };
                    if let Some(base_dir) = base_dir {
                        let abs_path = base_dir.join(rel_path);
                        open_terminal_at(&abs_path.to_string_lossy(), &label, toast_manager);
                    } else {
                        toast_manager.push(
                            "No active worktree context for submodule terminal".to_string(),
                            ToastSeverity::Error,
                        );
                    }
                } else {
                    toast_manager.push(
                        format!("Submodule '{}' not found", param),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        "open_worktree" => {
            if !param.is_empty() {
                let path = view_state
                    .worktree_state
                    .worktrees
                    .iter()
                    .find(|w| w.name == param)
                    .map(|w| w.path.clone());
                if let Some(path) = path {
                    open_terminal_at(&path, param, toast_manager);
                } else {
                    toast_manager.push(
                        format!("Worktree '{}' not found", param),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        "switch_worktree" => {
            if !param.is_empty() {
                view_state.switch_to_worktree_by_name(param, repo);
            }
        }
        "jump_to_worktree" => {
            if !param.is_empty() {
                view_state
                    .pending_messages
                    .push(AppMessage::JumpToWorktreeBranch(param.to_string()));
            }
        }
        "remove_worktree" => {
            if !param.is_empty() {
                let is_dirty = view_state
                    .worktree_state
                    .worktrees
                    .iter()
                    .any(|w| w.name == param && w.is_dirty);
                let msg = if is_dirty {
                    format!(
                        "Remove worktree '{}'? This worktree has uncommitted changes that will be lost.",
                        param
                    )
                } else {
                    format!("Remove worktree '{}'?", param)
                };
                confirm_dialog.show("Remove Worktree", &msg);
                *active_modal = Some(ActiveModal::Confirm);
                *pending_confirm_action = Some(AppMessage::RemoveWorktree(param.to_string()));
            }
        }
        "merge" => {
            if !param.is_empty() {
                let r = view_state.worktree_state.staging_repo_or(repo);
                let target_dir = view_state.worktree_state.selected_path.clone();
                if let Some(label) = crate::git::repo_state_label(r.repo_state()) {
                    toast_manager.push(
                        format!("Cannot merge: {}. Abort or complete it first.", label),
                        ToastSeverity::Error,
                    );
                } else {
                    let current = r.current_branch().unwrap_or_else(|_| "HEAD".to_string());
                    let uncommitted = r.uncommitted_change_count();
                    merge_dialog.show_with_target(param, &current, uncommitted, target_dir);
                    *active_modal = Some(ActiveModal::Merge);
                }
            }
        }
        "rebase" => {
            if !param.is_empty() {
                let r = view_state.worktree_state.staging_repo_or(repo);
                let target_dir = view_state.worktree_state.selected_path.clone();
                if let Some(label) = crate::git::repo_state_label(r.repo_state()) {
                    toast_manager.push(
                        format!("Cannot rebase: {}. Abort or complete it first.", label),
                        ToastSeverity::Error,
                    );
                } else {
                    let current = r.current_branch().unwrap_or_else(|_| "HEAD".to_string());
                    let uncommitted = r.uncommitted_change_count();
                    rebase_dialog.show_with_target(param, &current, uncommitted, target_dir);
                    *active_modal = Some(ActiveModal::Rebase);
                }
            }
        }
        "cherry_pick" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                let target_dir = view_state.worktree_state.selected_path.clone();
                let branch = view_state.current_branch_opt().unwrap_or("HEAD");
                confirm_dialog.show(
                    "Cherry-pick",
                    &format!(
                        "Cherry-pick commit {} into '{}' in {}?",
                        short, branch, operation_scope
                    ),
                );
                *active_modal = Some(ActiveModal::Confirm);
                *pending_confirm_action = Some(AppMessage::CherryPick(oid, target_dir));
            }
        }
        "revert_commit" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                let target_dir = view_state.worktree_state.selected_path.clone();
                let branch = view_state.current_branch_opt().unwrap_or("HEAD");
                confirm_dialog.show(
                    "Revert Commit",
                    &format!(
                        "Create a revert of {} on '{}' in {}?",
                        short, branch, operation_scope
                    ),
                );
                *active_modal = Some(ActiveModal::Confirm);
                *pending_confirm_action = Some(AppMessage::RevertCommit(oid, target_dir));
            }
        }
        "reset_soft" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                let target_dir = view_state.worktree_state.selected_path.clone();
                let branch = view_state.current_branch_opt().unwrap_or("HEAD");
                confirm_dialog.show(
                    "Reset (Soft)",
                    &format!(
                        "Reset '{}' to {} in {}? Changes will be kept staged.",
                        branch, short, operation_scope
                    ),
                );
                *active_modal = Some(ActiveModal::Confirm);
                *pending_confirm_action = Some(AppMessage::ResetToCommit(
                    oid,
                    git2::ResetType::Soft,
                    target_dir,
                ));
            }
        }
        "reset_mixed" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                let target_dir = view_state.worktree_state.selected_path.clone();
                let branch = view_state.current_branch_opt().unwrap_or("HEAD");
                confirm_dialog.show(
                    "Reset (Mixed)",
                    &format!(
                        "Reset '{}' to {} in {}? Changes will be kept unstaged.",
                        branch, short, operation_scope
                    ),
                );
                *active_modal = Some(ActiveModal::Confirm);
                *pending_confirm_action = Some(AppMessage::ResetToCommit(
                    oid,
                    git2::ResetType::Mixed,
                    target_dir,
                ));
            }
        }
        "reset_hard" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                let target_dir = view_state.worktree_state.selected_path.clone();
                let branch = view_state.current_branch_opt().unwrap_or("HEAD");
                confirm_dialog.show(
                    "Reset (Hard)",
                    &format!(
                        "Reset '{}' to {} in {}?\n\nALL changes will be DISCARDED. This cannot be undone.",
                        branch, short, operation_scope
                    ),
                );
                *active_modal = Some(ActiveModal::Confirm);
                *pending_confirm_action = Some(AppMessage::ResetToCommit(
                    oid,
                    git2::ResetType::Hard,
                    target_dir,
                ));
            }
        }
        "create_branch" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                let default_name = format!("branch-{}", short);
                branch_name_dialog.show(&default_name, oid);
                *active_modal = Some(ActiveModal::BranchName);
            }
        }
        "create_worktree" => {
            let has_submodules = !view_state.staging_well.submodules.is_empty();
            let has_lfs = repo.has_lfs();
            if param.is_empty() {
                // From commit graph: use short SHA as source
                if let Some(oid) = view_state.context_menu_commit {
                    let short = &oid.to_string()[..7];
                    let default_name = format!("wt-{}", short);
                    branch_name_dialog.show_for_worktree(
                        &default_name,
                        short,
                        has_submodules,
                        has_lfs,
                    );
                    *active_modal = Some(ActiveModal::BranchName);
                }
            } else {
                // From branch sidebar: use branch name as source
                branch_name_dialog.show_for_worktree(param, param, has_submodules, has_lfs);
                *active_modal = Some(ActiveModal::BranchName);
            }
        }
        "create_tag" => {
            if let Some(oid) = view_state.context_menu_commit {
                let short = &oid.to_string()[..7];
                let default_name = format!("v0.1.0-{}", short);
                branch_name_dialog.show_with_title("Create Tag", &default_name, oid);
                *active_modal = Some(ActiveModal::BranchName);
            }
        }
        "delete_tag" => {
            if !param.is_empty() {
                confirm_dialog.show("Delete Tag", &format!("Delete tag '{}'?", param));
                *active_modal = Some(ActiveModal::Confirm);
                *pending_confirm_action = Some(AppMessage::DeleteTag(param.to_string()));
            }
        }
        "stash_push" => {
            view_state.pending_messages.push(AppMessage::StashPush);
        }
        "stash_pop" => {
            view_state.pending_messages.push(AppMessage::StashPop);
        }
        "apply_stash" => {
            if let Ok(index) = param.parse::<usize>() {
                view_state
                    .pending_messages
                    .push(AppMessage::StashApply(index));
            }
        }
        "pop_stash" => {
            if let Ok(index) = param.parse::<usize>() {
                view_state
                    .pending_messages
                    .push(AppMessage::StashPopIndex(index));
            }
        }
        "drop_stash" => {
            if let Ok(index) = param.parse::<usize>() {
                confirm_dialog.show(
                    "Drop Stash",
                    &format!("Drop stash@{{{}}}? This cannot be undone.", index),
                );
                *active_modal = Some(ActiveModal::Confirm);
                *pending_confirm_action = Some(AppMessage::StashDrop(index));
            }
        }
        "fetch_all" => {
            view_state.pending_messages.push(AppMessage::FetchAll);
        }
        "add_remote" => {
            remote_dialog.show_add();
            *active_modal = Some(ActiveModal::Remote);
        }
        "edit_remote_url" => {
            if !param.is_empty() {
                let current_url = repo.remote_url(param).unwrap_or_default();
                remote_dialog.show_edit_url(param, &current_url);
                *active_modal = Some(ActiveModal::Remote);
            }
        }
        "rename_remote" => {
            if !param.is_empty() {
                remote_dialog.show_rename(param);
                *active_modal = Some(ActiveModal::Remote);
            }
        }
        "delete_remote" => {
            if !param.is_empty() {
                confirm_dialog.show("Delete Remote", &format!("Delete remote '{}'? This will remove all remote-tracking branches for this remote.", param));
                *active_modal = Some(ActiveModal::Confirm);
                *pending_confirm_action = Some(AppMessage::DeleteRemote(param.to_string()));
            }
        }
        "merge_remote" => {
            if !param.is_empty() {
                let r = view_state.worktree_state.staging_repo_or(repo);
                let target_dir = view_state.worktree_state.selected_path.clone();
                if let Some(label) = crate::git::repo_state_label(r.repo_state()) {
                    toast_manager.push(
                        format!("Cannot merge: {}. Abort or complete it first.", label),
                        ToastSeverity::Error,
                    );
                } else {
                    let current = r.current_branch().unwrap_or_else(|_| "HEAD".to_string());
                    let uncommitted = r.uncommitted_change_count();
                    merge_dialog.show_with_target(param, &current, uncommitted, target_dir);
                    *active_modal = Some(ActiveModal::Merge);
                }
            }
        }
        "rebase_remote" => {
            if !param.is_empty() {
                let r = view_state.worktree_state.staging_repo_or(repo);
                let target_dir = view_state.worktree_state.selected_path.clone();
                if let Some(label) = crate::git::repo_state_label(r.repo_state()) {
                    toast_manager.push(
                        format!("Cannot rebase: {}. Abort or complete it first.", label),
                        ToastSeverity::Error,
                    );
                } else {
                    let current = r.current_branch().unwrap_or_else(|_| "HEAD".to_string());
                    let uncommitted = r.uncommitted_change_count();
                    rebase_dialog.show_with_target(param, &current, uncommitted, target_dir);
                    *active_modal = Some(ActiveModal::Rebase);
                }
            }
        }
        "delete_remote_branch" => {
            if !param.is_empty()
                && let Some((remote, branch)) = param.split_once('/')
            {
                confirm_dialog.show(
                    "Delete Remote Branch",
                    &format!(
                        "Delete branch '{}' from remote '{}'? This cannot be undone.",
                        branch, remote
                    ),
                );
                *active_modal = Some(ActiveModal::Confirm);
                *pending_confirm_action = Some(AppMessage::DeleteRemoteBranch(
                    remote.to_string(),
                    branch.to_string(),
                ));
            }
        }
        "checkout_in_wt" => {
            // Format: "checkout_in_wt:branch|wt_name" — from context menu
            if !param.is_empty()
                && let Some((branch, wt_name)) = param.split_once('|')
            {
                if let Some(wt) = view_state
                    .worktree_state
                    .worktrees
                    .iter()
                    .find(|w| w.name == wt_name)
                {
                    view_state
                        .pending_messages
                        .push(AppMessage::CheckoutBranchInWorktree(
                            branch.to_string(),
                            PathBuf::from(&wt.path),
                        ));
                } else {
                    toast_manager.push(
                        format!("Worktree '{}' not found", wt_name),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        "set_head" => {
            if !param.is_empty() {
                view_state
                    .pending_messages
                    .push(AppMessage::SetHead(param.to_string()));
            }
        }
        _ => {
            toast_manager.push(
                format!("Unknown action: {}", action_id),
                ToastSeverity::Error,
            );
        }
    }

    view_state.context_menu_commit = None;
}

/// Add panel backgrounds, borders, and visual chrome to the output.
/// `mouse_pos` is used to highlight dividers on hover for drag affordance.
#[allow(clippy::too_many_arguments)]
fn add_panel_chrome(
    output: &mut WidgetOutput,
    layout: &ScreenLayout,
    screen_bounds: &Rect,
    focused: FocusedPanel,
    mouse_pos: (f32, f32),
    staging_mode: bool,
    staging_preview_ratio: f32,
    pill_bar_h: f32,
) {
    // Panel backgrounds for depth separation
    output
        .spline_vertices
        .extend(crate::ui::widget::create_rect_vertices(
            &layout.graph,
            theme::PANEL_GRAPH.to_array(),
        ));
    output
        .spline_vertices
        .extend(crate::ui::widget::create_rect_vertices(
            &layout.right_panel,
            theme::PANEL_STAGING.to_array(),
        ));

    // Border below shortcut bar (full width of screen)
    output
        .spline_vertices
        .extend(crate::ui::widget::create_rect_vertices(
            &Rect::new(0.0, layout.shortcut_bar.bottom(), screen_bounds.width, 1.0),
            theme::BORDER.to_array(),
        ));

    // Divider hover detection: brighten divider when mouse is within 8px (matches drag hit zone)
    let (mx, my) = mouse_pos;
    let hit_tolerance = 8.0;
    let in_content_area = my > layout.shortcut_bar.bottom();

    let sidebar_edge = layout.sidebar.right();
    let sidebar_graph_hover = in_content_area && (mx - sidebar_edge).abs() < hit_tolerance;

    let graph_edge = layout.graph.right();
    let graph_right_hover = in_content_area && (mx - graph_edge).abs() < hit_tolerance;

    // Vertical divider: sidebar | graph
    // Visible 2px line at rest, wider 3px highlighted line on hover
    if sidebar_graph_hover {
        output
            .spline_vertices
            .extend(crate::ui::widget::create_rect_vertices(
                &Rect::new(
                    layout.sidebar.right(),
                    layout.sidebar.y,
                    3.0,
                    layout.sidebar.height,
                ),
                theme::BORDER_LIGHT.to_array(),
            ));
    } else {
        output
            .spline_vertices
            .extend(crate::ui::widget::create_rect_vertices(
                &Rect::new(
                    layout.sidebar.right(),
                    layout.sidebar.y,
                    2.0,
                    layout.sidebar.height,
                ),
                theme::BORDER.with_alpha(0.50).to_array(),
            ));
    }

    // Vertical divider: graph | right panel
    if graph_right_hover {
        output
            .spline_vertices
            .extend(crate::ui::widget::create_rect_vertices(
                &Rect::new(
                    layout.graph.right(),
                    layout.graph.y,
                    3.0,
                    layout.graph.height,
                ),
                theme::BORDER_LIGHT.to_array(),
            ));
    } else {
        output
            .spline_vertices
            .extend(crate::ui::widget::create_rect_vertices(
                &Rect::new(
                    layout.graph.right(),
                    layout.graph.y,
                    2.0,
                    layout.graph.height,
                ),
                theme::BORDER.with_alpha(0.50).to_array(),
            ));
    }

    // Horizontal divider: staging | preview (within right panel, staging mode only)
    if staging_mode {
        let (_, content_rect) = layout.right_panel.take_top(pill_bar_h);
        let split_y = content_rect.y + content_rect.height * staging_preview_ratio;
        let hit_tolerance = 8.0;
        let staging_preview_hover =
            layout.right_panel.contains(mx, my) && (my - split_y).abs() < hit_tolerance;

        if staging_preview_hover {
            output
                .spline_vertices
                .extend(crate::ui::widget::create_rect_vertices(
                    &Rect::new(
                        layout.right_panel.x,
                        split_y - 1.0,
                        layout.right_panel.width,
                        3.0,
                    ),
                    theme::BORDER_LIGHT.to_array(),
                ));
        } else {
            output
                .spline_vertices
                .extend(crate::ui::widget::create_rect_vertices(
                    &Rect::new(layout.right_panel.x, split_y, layout.right_panel.width, 2.0),
                    theme::BORDER.with_alpha(0.50).to_array(),
                ));
        }
    }

    // Focused panel indicator: accent-colored top border (3px at ~60% alpha)
    let focused_rect = match focused {
        FocusedPanel::Graph => &layout.graph,
        FocusedPanel::RightPanel => &layout.right_panel,
        FocusedPanel::Sidebar => &layout.sidebar,
    };
    output
        .spline_vertices
        .extend(crate::ui::widget::create_rect_vertices(
            &Rect::new(focused_rect.x, focused_rect.y, focused_rect.width, 3.0),
            theme::ACCENT.with_alpha(0.6).to_array(),
        ));
}

/// Build the UI vertices for the active tab.
/// Takes separate borrows to avoid conflict between App fields and RenderState.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_ui_output(
    tabs: &mut [(RepoTab, TabViewState)],
    active_tab: usize,
    tab_bar: &TabBar,
    toast_manager: &mut ToastManager,
    tooltip: &mut Tooltip,
    active_modal: Option<ActiveModal>,
    repo_dialog: &RepoDialog,
    clone_dialog: &CloneDialog,
    settings_dialog: &SettingsDialog,
    token_dialog: &TokenDialog,
    confirm_dialog: &ConfirmDialog,
    error_dialog: &ErrorDialog,
    branch_name_dialog: &BranchNameDialog,
    remote_dialog: &RemoteDialog,
    merge_dialog: &MergeDialog,
    rebase_dialog: &RebaseDialog,
    pull_dialog: &PullDialog,
    push_dialog: &PushDialog,
    text_renderer: &TextRenderer,
    bold_text_renderer: &TextRenderer,
    scale_factor: f64,
    extent: [u32; 2],
    avatar_cache: &mut AvatarCache,
    avatar_renderer: &AvatarRenderer,
    icon_renderer: &IconRenderer,
    sidebar_ratio: f32,
    graph_ratio: f32,
    staging_preview_ratio: f32,
    shortcut_bar_visible: bool,
    mouse_pos: (f32, f32),
    elapsed: f32,
) -> (WidgetOutput, WidgetOutput, WidgetOutput) {
    let screen_bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
    let scale = scale_factor as f32;

    // Tab bar takes space at top when multiple tabs
    let tab_bar_height = if tabs.len() > 1 {
        TabBar::height(scale)
    } else {
        0.0
    };
    let (tab_bar_bounds, main_bounds) = screen_bounds.take_top(tab_bar_height);
    let layout = ScreenLayout::compute_with_ratios_and_shortcut(
        main_bounds,
        4.0,
        scale,
        Some(sidebar_ratio),
        Some(graph_ratio),
        shortcut_bar_visible,
    );

    // Three layers: graph content renders first, chrome on top, overlay on top of everything
    let mut graph_output = WidgetOutput::new();
    let mut chrome_output = WidgetOutput::new();
    let mut overlay_output = WidgetOutput::new();

    // Panel backgrounds and borders go in graph layer (base - renders first, behind everything)
    let focused = tabs
        .get(active_tab)
        .map(|(_, vs)| vs.focused_panel)
        .unwrap_or_default();
    let staging_mode = tabs
        .get(active_tab)
        .map(|(_, vs)| vs.right_panel_mode == RightPanelMode::Staging)
        .unwrap_or(false);
    let pill_bar_h = tabs
        .get(active_tab)
        .map(|(_, vs)| vs.staging_well.pill_bar_height(&vs.current_branch))
        .unwrap_or(0.0);
    add_panel_chrome(
        &mut graph_output,
        &layout,
        &main_bounds,
        focused,
        mouse_pos,
        staging_mode,
        staging_preview_ratio,
        pill_bar_h,
    );

    // Active tab views
    if let Some((repo_tab, view_state)) = tabs.get_mut(active_tab) {
        // Commit graph (graph layer - renders first)
        let spline_vertices = view_state.commit_graph_view.layout_splines(
            text_renderer,
            &repo_tab.commits,
            layout.graph,
            view_state.head_oid,
        );
        let (text_vertices, pill_vertices, av_vertices) = view_state.commit_graph_view.layout_text(
            text_renderer,
            &repo_tab.commits,
            layout.graph,
            avatar_cache,
            avatar_renderer,
            view_state.head_oid,
            &view_state.worktree_state.worktrees,
        );
        graph_output.spline_vertices.extend(spline_vertices);
        graph_output.spline_vertices.extend(pill_vertices);
        graph_output.text_vertices.extend(text_vertices);
        graph_output.avatar_vertices.extend(av_vertices);

        // Offer tooltips for truncated commit subjects (uses current frame's data).
        // Suppress when any dialog or context menu is open.
        let any_overlay_open = active_modal.is_some() || view_state.context_menu.is_visible();
        tooltip.begin_frame();
        if !any_overlay_open {
            let (mx, my) = mouse_pos;
            if layout.graph.contains(mx, my) {
                for (text_bounds, full_text) in &view_state.commit_graph_view.truncated_subjects {
                    if text_bounds.contains(mx, my) {
                        tooltip.offer(*text_bounds, full_text, mx, my);
                        break;
                    }
                }
                for (badge_bounds, hidden_labels) in
                    &view_state.commit_graph_view.overflow_pill_tooltips
                {
                    if badge_bounds.contains(mx, my) {
                        tooltip.offer(*badge_bounds, hidden_labels, mx, my);
                        break;
                    }
                }
            }
            // Header bar truncated buttons
            view_state
                .header_bar
                .report_tooltip(tooltip, mx, my, layout.header);
        }
        tooltip.end_frame();

        // Opaque header backdrop to prevent graph bleed-through between tab bar and header
        let header_backdrop_h = layout.header.height
            + if shortcut_bar_visible {
                layout.shortcut_bar.height
            } else {
                0.0
            };
        chrome_output
            .spline_vertices
            .extend(crate::ui::widget::create_rect_vertices(
                &Rect::new(
                    main_bounds.x,
                    main_bounds.y,
                    main_bounds.width,
                    header_backdrop_h,
                ),
                theme::SURFACE_RAISED.to_array(),
            ));

        // Header bar (chrome layer - on top of graph)
        chrome_output.extend(view_state.header_bar.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            layout.header,
            elapsed,
            Some(icon_renderer),
        ));

        // Shortcut bar (chrome layer - on top of graph) - only when visible
        if shortcut_bar_visible {
            chrome_output.extend(
                view_state
                    .shortcut_bar
                    .layout(text_renderer, layout.shortcut_bar),
            );
        }

        // Branch sidebar (chrome layer)
        chrome_output.extend(view_state.branch_sidebar.layout(
            text_renderer,
            bold_text_renderer,
            layout.sidebar,
            &view_state.current_branch,
            Some(icon_renderer),
        ));

        // Right panel (chrome layer) - worktree pills + mode-dependent content
        {
            let pill_bar_h = view_state
                .staging_well
                .pill_bar_height(&view_state.current_branch);
            let (pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);

            // Worktree pill bar (visible when there are worktree contexts)
            if pill_bar_h > 0.0 {
                chrome_output.extend(view_state.staging_well.layout_worktree_pills(
                    text_renderer,
                    pill_rect,
                    &view_state.current_branch,
                    &view_state.worktree_state.worktrees,
                ));
            }

            match view_state.right_panel_mode {
                RightPanelMode::Staging => {
                    // Upper: staging well, Lower: diff view with header
                    let (staging_rect, diff_rect) =
                        content_rect.split_vertical(staging_preview_ratio);
                    chrome_output
                        .extend(view_state.staging_well.layout(text_renderer, staging_rect));

                    let has_diff = view_state.diff_view.has_content();
                    let title = if has_diff {
                        view_state.diff_view.title()
                    } else {
                        "Preview"
                    };
                    let diff_body_rect = render_preview_header(
                        &mut chrome_output,
                        diff_rect,
                        title,
                        !has_diff,
                        scale,
                        bold_text_renderer,
                    );

                    if has_diff {
                        chrome_output
                            .extend(view_state.diff_view.layout(text_renderer, diff_body_rect));
                    } else {
                        let msg = "Select a file to preview its diff";
                        let msg_w = text_renderer.measure_text(msg);
                        let line_h = text_renderer.line_height();
                        let cx = diff_body_rect.x + (diff_body_rect.width - msg_w) / 2.0;
                        let cy = diff_body_rect.y + (diff_body_rect.height - line_h) / 2.0;
                        chrome_output
                            .text_vertices
                            .extend(text_renderer.layout_text(
                                msg,
                                cx,
                                cy,
                                theme::TEXT_MUTED.to_array(),
                            ));
                    }
                }
                RightPanelMode::Browse => {
                    // Upper: commit detail, Lower: diff view with header
                    if view_state.commit_detail_view.has_content() {
                        let (detail_rect, diff_rect) = content_rect.split_vertical(0.40);
                        chrome_output.extend(
                            view_state
                                .commit_detail_view
                                .layout(text_renderer, detail_rect),
                        );

                        let has_diff = view_state.diff_view.has_content();
                        let title = if has_diff {
                            view_state.diff_view.title()
                        } else {
                            "Diff"
                        };
                        let diff_body_rect = render_preview_header(
                            &mut chrome_output,
                            diff_rect,
                            title,
                            !has_diff,
                            scale,
                            bold_text_renderer,
                        );

                        if has_diff {
                            chrome_output
                                .extend(view_state.diff_view.layout(text_renderer, diff_body_rect));
                        }
                    } else if view_state.diff_view.has_content() {
                        let title = view_state.diff_view.title();
                        let diff_body_rect = render_preview_header(
                            &mut chrome_output,
                            content_rect,
                            title,
                            false,
                            scale,
                            bold_text_renderer,
                        );
                        chrome_output
                            .extend(view_state.diff_view.layout(text_renderer, diff_body_rect));
                    } else {
                        let body_rect = render_preview_header(
                            &mut chrome_output,
                            content_rect,
                            "Preview",
                            true,
                            scale,
                            bold_text_renderer,
                        );
                        let msg = "Select a commit to browse";
                        let msg_w = text_renderer.measure_text(msg);
                        let line_h = text_renderer.line_height();
                        let cx = body_rect.x + (body_rect.width - msg_w) / 2.0;
                        let cy = body_rect.y + (body_rect.height - line_h) / 2.0;
                        chrome_output
                            .text_vertices
                            .extend(text_renderer.layout_text(
                                msg,
                                cx,
                                cy,
                                theme::TEXT_MUTED.to_array(),
                            ));
                    }
                }
            }
        }
    }

    // Tab bar (chrome layer - rendered after graph so it draws on top)
    if tabs.len() > 1 {
        chrome_output.extend(tab_bar.layout(text_renderer, tab_bar_bounds));
    }

    // Context menu overlay (overlay layer - on top of all panels)
    if let Some((_, view_state)) = tabs.get_mut(active_tab)
        && view_state.context_menu.is_visible()
    {
        overlay_output.extend(view_state.context_menu.layout(text_renderer, screen_bounds));
    }

    // Toast notifications (overlay layer - on top of context menus)
    overlay_output.extend(toast_manager.layout(text_renderer, screen_bounds, scale));

    // Tooltip (overlay layer - on top of toasts, below dialogs)
    overlay_output.extend(tooltip.layout(text_renderer, screen_bounds, scale));

    // Repo dialog (overlay layer - on top of everything including toasts)
    if repo_dialog.is_visible() {
        overlay_output.extend(repo_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    // Clone dialog (overlay layer)
    if clone_dialog.is_visible() {
        overlay_output.extend(clone_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    // Settings dialog (overlay layer - on top of everything)
    if settings_dialog.is_visible() {
        overlay_output.extend(settings_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    // Token dialog (overlay layer - on top of settings)
    if token_dialog.is_visible() {
        overlay_output.extend(token_dialog.layout(text_renderer, screen_bounds));
    }

    // Confirm dialog (overlay layer - on top of everything including settings)
    if confirm_dialog.is_visible() {
        overlay_output.extend(confirm_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    // Error dialog (overlay layer - on top of confirm dialog)
    if error_dialog.is_visible() {
        overlay_output.extend(error_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    // Branch name dialog (overlay layer - on top of everything)
    if branch_name_dialog.is_visible() {
        overlay_output.extend(branch_name_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    // Remote dialog (overlay layer - on top of everything)
    if remote_dialog.is_visible() {
        overlay_output.extend(remote_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    // Pull dialog (overlay layer - on top of everything)
    if pull_dialog.is_visible() {
        overlay_output.extend(pull_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    if push_dialog.is_visible() {
        overlay_output.extend(push_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    // Merge dialog (overlay layer - on top of everything)
    if merge_dialog.is_visible() {
        overlay_output.extend(merge_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    // Rebase dialog (overlay layer - on top of everything)
    if rebase_dialog.is_visible() {
        overlay_output.extend(rebase_dialog.layout_with_bold(
            text_renderer,
            bold_text_renderer,
            screen_bounds,
        ));
    }

    (graph_output, chrome_output, overlay_output)
}

pub(crate) fn draw_frame(app: &mut App) -> Result<()> {
    let state = app.state.as_mut().unwrap();
    state
        .previous_frame_end
        .as_mut()
        .unwrap()
        .cleanup_finished();

    // Recreate swapchain if needed
    if state.surface.needs_recreate {
        state
            .surface
            .recreate(&state.ctx, state.window.inner_size())?;
    }

    // Acquire next image
    let (image_index, suboptimal, acquire_future) = match acquire_next_image(
        state.surface.swapchain.clone(),
        None,
    )
    .map_err(Validated::unwrap)
    {
        Ok(r) => r,
        Err(VulkanError::OutOfDate) => {
            state.surface.needs_recreate = true;
            return Ok(());
        }
        Err(e) => anyhow::bail!("Failed to acquire next image: {e:?}"),
    };

    if suboptimal {
        state.surface.needs_recreate = true;
    }

    // Sync button state and shortcut context before layout
    let single_tab = app.tabs.len() == 1;
    let now = Instant::now();
    let elapsed = app.app_start.elapsed().as_secs_f32();
    if let Some((_, view_state)) = app.tabs.get_mut(app.active_tab) {
        // Set generic op label from receiver (for spinner indicator in header)
        view_state.header_bar.generic_op_label =
            view_state
                .generic_op_receiver
                .as_ref()
                .map(|(_, label, _)| {
                    let dot_count = ((elapsed * 2.5) as usize % 3) + 1;
                    let dots: String = ".".repeat(dot_count);
                    format!("{}{}", label, dots)
                });
        view_state.header_bar.ci_results = view_state.ci_results.clone();
        let branch_opt = view_state.current_branch_opt().map(|s| s.to_string());
        view_state.header_bar.update_button_state(
            elapsed,
            branch_opt.as_deref(),
            &state.bold_text_renderer,
        );
        view_state.staging_well.update_button_state(elapsed);
        view_state.staging_well.update_cursors(now);
        view_state.commit_graph_view.search_bar.update_cursor(now);
        view_state.branch_sidebar.update_filter_cursor(now);
        view_state
            .shortcut_bar
            .set_context(match view_state.focused_panel {
                FocusedPanel::Graph => ShortcutContext::Graph,
                FocusedPanel::RightPanel => match view_state.right_panel_mode {
                    RightPanelMode::Staging => ShortcutContext::Staging,
                    RightPanelMode::Browse => ShortcutContext::Graph,
                },
                FocusedPanel::Sidebar => ShortcutContext::Sidebar,
            });
        view_state.shortcut_bar.show_new_tab_hint = single_tab;

        // Sync breadcrumb data from submodule focus state
        if let Some(ref focus) = view_state.submodule_focus {
            let home = std::env::var("HOME").unwrap_or_default();
            let mut segs: Vec<String> = Vec::new();
            for (i, s) in focus.parent_stack.iter().enumerate() {
                if i == 0 {
                    // First segment: show abbreviated repo path for the root repo
                    let root_path = s
                        .repo
                        .workdir()
                        .or_else(|| Some(s.repo.git_dir()))
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|| s.repo_name.clone());
                    let root_path = root_path.trim_end_matches('/').to_string();
                    if !home.is_empty() && root_path.starts_with(&home) {
                        segs.push(format!("~{}", &root_path[home.len()..]));
                    } else {
                        segs.push(root_path);
                    }
                } else {
                    // Intermediate segments: submodule names
                    segs.push(s.submodule_name.clone());
                }
            }
            segs.push(focus.current_name.clone());
            view_state.header_bar.breadcrumb_segments = segs;
        } else {
            view_state.header_bar.breadcrumb_segments.clear();
        }

        // Pre-compute breadcrumb segment bounds for hit testing
        // (needs approximate header bounds — compute from extent)
        let extent = state.surface.extent();
        let screen_bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
        let tab_bar_height = if single_tab {
            0.0
        } else {
            TabBar::height(state.scale_factor as f32)
        };
        let (_tb_bounds, main_bounds) = screen_bounds.take_top(tab_bar_height);
        let approx_layout = ScreenLayout::compute_with_ratios_and_shortcut(
            main_bounds,
            4.0,
            state.scale_factor as f32,
            Some(app.sidebar_ratio),
            Some(app.graph_ratio),
            app.shortcut_bar_visible,
        );
        view_state
            .header_bar
            .update_breadcrumb_bounds(&state.text_renderer, approx_layout.header);
        view_state
            .header_bar
            .update_abort_bounds(&state.text_renderer, approx_layout.header);
        view_state
            .header_bar
            .update_ci_bounds(&state.text_renderer, approx_layout.header);
    }

    // Update toast manager and tooltip
    app.toast_manager.update(Instant::now());
    app.tooltip.update();

    // Poll avatar downloads and pack newly loaded ones into the atlas
    let newly_loaded = state.avatar_cache.poll_downloads();
    for email in &newly_loaded {
        if let Some((rgba, size)) = state.avatar_cache.get_loaded(email) {
            state.avatar_renderer.pack_avatar(email, rgba, size);
        }
    }

    let extent = state.surface.extent();
    let scale_factor = state.scale_factor;
    let mouse_pos = state.input_state.mouse.position();
    let (sidebar_ratio, graph_ratio) = (app.sidebar_ratio, app.graph_ratio);
    let elapsed = app.app_start.elapsed().as_secs_f32();
    let (graph_output, chrome_output, overlay_output) = build_ui_output(
        &mut app.tabs,
        app.active_tab,
        &app.tab_bar,
        &mut app.toast_manager,
        &mut app.tooltip,
        app.active_modal,
        &app.repo_dialog,
        &app.clone_dialog,
        &app.settings_dialog,
        &app.token_dialog,
        &app.confirm_dialog,
        &app.error_dialog,
        &app.branch_name_dialog,
        &app.remote_dialog,
        &app.merge_dialog,
        &app.rebase_dialog,
        &app.pull_dialog,
        &app.push_dialog,
        &state.text_renderer,
        &state.bold_text_renderer,
        scale_factor,
        extent,
        &mut state.avatar_cache,
        &state.avatar_renderer,
        &state.icon_renderer,
        sidebar_ratio,
        graph_ratio,
        app.staging_preview_ratio,
        app.shortcut_bar_visible,
        mouse_pos,
        elapsed,
    );

    let viewport = Viewport {
        offset: [0.0, 0.0],
        extent: [extent[0] as f32, extent[1] as f32],
        depth_range: 0.0..=1.0,
    };

    // Upload avatar/icon atlas in a separate command buffer if dirty
    if state.avatar_renderer.needs_upload() || state.icon_renderer.needs_upload() {
        let mut upload_builder = AutoCommandBufferBuilder::primary(
            state.ctx.command_buffer_allocator.clone(),
            state.ctx.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .context("Failed to create upload command buffer")?;

        state.avatar_renderer.upload_atlas(&mut upload_builder)?;
        state.icon_renderer.upload_atlas(&mut upload_builder)?;

        let upload_cb = upload_builder
            .build()
            .context("Failed to build upload command buffer")?;
        let upload_future = state
            .previous_frame_end
            .take()
            .unwrap()
            .then_execute(state.ctx.queue.clone(), upload_cb)
            .context("Failed to execute upload")?
            .then_signal_fence_and_flush()
            .map_err(Validated::unwrap)
            .context("Failed to flush upload")?;
        upload_future
            .wait(None)
            .context("Failed to wait for upload")?;
        state.previous_frame_end = Some(sync::now(state.ctx.device.clone()).boxed());
    }

    // Build command buffer
    let mut builder = AutoCommandBufferBuilder::primary(
        state.ctx.command_buffer_allocator.clone(),
        state.ctx.queue.queue_family_index(),
        CommandBufferUsage::OneTimeSubmit,
    )
    .context("Failed to create command buffer")?;

    builder
        .begin_render_pass(
            RenderPassBeginInfo {
                clear_values: vec![
                    Some(clear_color_for_format(state.surface.image_format()).into()),
                    None,
                ],
                ..RenderPassBeginInfo::framebuffer(
                    state.surface.framebuffers[image_index as usize].clone(),
                )
            },
            Default::default(),
        )
        .context("Failed to begin render pass")?;

    render_output_to_builder(&mut builder, state, graph_output, viewport.clone())?;
    render_output_to_builder(&mut builder, state, chrome_output, viewport.clone())?;
    render_output_to_builder(&mut builder, state, overlay_output, viewport)?;

    builder
        .end_render_pass(Default::default())
        .context("Failed to end render pass")?;

    let command_buffer = builder.build().context("Failed to build command buffer")?;

    // Submit
    let future = state
        .previous_frame_end
        .take()
        .unwrap()
        .join(acquire_future)
        .then_execute(state.ctx.queue.clone(), command_buffer)
        .context("Failed to execute")?
        .then_swapchain_present(
            state.ctx.queue.clone(),
            SwapchainPresentInfo::swapchain_image_index(
                state.surface.swapchain.clone(),
                image_index,
            ),
        )
        .then_signal_fence_and_flush();

    match future.map_err(Validated::unwrap) {
        Ok(future) => state.previous_frame_end = Some(future.boxed()),
        Err(VulkanError::OutOfDate) => {
            state.surface.needs_recreate = true;
            state.previous_frame_end = Some(sync::now(state.ctx.device.clone()).boxed());
        }
        Err(e) => {
            state.previous_frame_end = Some(sync::now(state.ctx.device.clone()).boxed());
            anyhow::bail!("Failed to flush: {e:?}");
        }
    }

    state.frame_count += 1;
    Ok(())
}

#[inline]
fn srgb_to_linear_channel(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn clear_color_for_format(format: Format) -> [f32; 4] {
    let bg = theme::BACKGROUND.to_array();
    if format.numeric_format_color() == Some(NumericFormat::SRGB) {
        [
            srgb_to_linear_channel(bg[0]),
            srgb_to_linear_channel(bg[1]),
            srgb_to_linear_channel(bg[2]),
            bg[3],
        ]
    } else {
        bg
    }
}

/// Draw the UI output into a command buffer builder (shared by all render paths).
fn render_output_to_builder(
    builder: &mut AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
    state: &RenderState,
    output: WidgetOutput,
    viewport: Viewport,
) -> Result<()> {
    if !output.spline_vertices.is_empty() {
        let spline_buffer = state
            .spline_renderer
            .create_vertex_buffer(output.spline_vertices)?;
        state
            .spline_renderer
            .draw(builder, spline_buffer, viewport.clone())?;
    }
    if !output.avatar_vertices.is_empty() {
        let avatar_buffer = state
            .avatar_renderer
            .create_vertex_buffer(output.avatar_vertices)?;
        state
            .avatar_renderer
            .draw(builder, avatar_buffer, viewport.clone())?;
    }
    if !output.icon_vertices.is_empty() {
        let icon_buffer = state
            .icon_renderer
            .create_vertex_buffer(output.icon_vertices)?;
        state
            .icon_renderer
            .draw(builder, icon_buffer, viewport.clone())?;
    }
    if !output.text_vertices.is_empty() {
        let vertex_buffer = state
            .text_renderer
            .create_vertex_buffer(output.text_vertices)?;
        state
            .text_renderer
            .draw(builder, vertex_buffer, viewport.clone())?;
    }
    if !output.bold_text_vertices.is_empty() {
        let bold_buffer = state
            .bold_text_renderer
            .create_vertex_buffer(output.bold_text_vertices)?;
        state
            .bold_text_renderer
            .draw(builder, bold_buffer, viewport)?;
    }
    Ok(())
}

pub(crate) fn capture_screenshot(app: &mut App) -> Result<image::RgbaImage> {
    let state = app.state.as_mut().unwrap();
    state
        .previous_frame_end
        .as_mut()
        .unwrap()
        .cleanup_finished();

    let extent = state.surface.extent();
    let scale_factor = state.scale_factor;
    let (sidebar_ratio, graph_ratio) = (app.sidebar_ratio, app.graph_ratio);
    let elapsed = app.app_start.elapsed().as_secs_f32();
    let (graph_output, chrome_output, overlay_output) = build_ui_output(
        &mut app.tabs,
        app.active_tab,
        &app.tab_bar,
        &mut app.toast_manager,
        &mut app.tooltip,
        app.active_modal,
        &app.repo_dialog,
        &app.clone_dialog,
        &app.settings_dialog,
        &app.token_dialog,
        &app.confirm_dialog,
        &app.error_dialog,
        &app.branch_name_dialog,
        &app.remote_dialog,
        &app.merge_dialog,
        &app.rebase_dialog,
        &app.pull_dialog,
        &app.push_dialog,
        &state.text_renderer,
        &state.bold_text_renderer,
        scale_factor,
        extent,
        &mut state.avatar_cache,
        &state.avatar_renderer,
        &state.icon_renderer,
        sidebar_ratio,
        graph_ratio,
        app.staging_preview_ratio,
        app.shortcut_bar_visible,
        (0.0, 0.0), // No mouse interaction for screenshots
        elapsed,
    );

    let state = app.state.as_mut().unwrap();
    let viewport = Viewport {
        offset: [0.0, 0.0],
        extent: [extent[0] as f32, extent[1] as f32],
        depth_range: 0.0..=1.0,
    };

    // Upload avatar/icon atlas in a separate command buffer if dirty
    if state.avatar_renderer.needs_upload() || state.icon_renderer.needs_upload() {
        let mut upload_builder = AutoCommandBufferBuilder::primary(
            state.ctx.command_buffer_allocator.clone(),
            state.ctx.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .context("Failed to create upload command buffer")?;

        state.avatar_renderer.upload_atlas(&mut upload_builder)?;
        state.icon_renderer.upload_atlas(&mut upload_builder)?;

        let upload_cb = upload_builder
            .build()
            .context("Failed to build upload command buffer")?;
        let upload_future = state
            .previous_frame_end
            .take()
            .unwrap()
            .then_execute(state.ctx.queue.clone(), upload_cb)
            .context("Failed to execute upload")?
            .then_signal_fence_and_flush()
            .map_err(Validated::unwrap)
            .context("Failed to flush upload")?;
        upload_future
            .wait(None)
            .context("Failed to wait for upload")?;
        state.previous_frame_end = Some(sync::now(state.ctx.device.clone()).boxed());
    }

    // Acquire image
    let (image_index, _, acquire_future) =
        acquire_next_image(state.surface.swapchain.clone(), None)
            .map_err(Validated::unwrap)
            .context("Failed to acquire image")?;

    let mut builder = AutoCommandBufferBuilder::primary(
        state.ctx.command_buffer_allocator.clone(),
        state.ctx.queue.queue_family_index(),
        CommandBufferUsage::OneTimeSubmit,
    )
    .context("Failed to create command buffer")?;

    builder
        .begin_render_pass(
            RenderPassBeginInfo {
                clear_values: vec![
                    Some(clear_color_for_format(state.surface.image_format()).into()),
                    None,
                ],
                ..RenderPassBeginInfo::framebuffer(
                    state.surface.framebuffers[image_index as usize].clone(),
                )
            },
            Default::default(),
        )
        .context("Failed to begin render pass")?;

    render_output_to_builder(&mut builder, state, graph_output, viewport.clone())?;
    render_output_to_builder(&mut builder, state, chrome_output, viewport.clone())?;
    render_output_to_builder(&mut builder, state, overlay_output, viewport)?;

    builder
        .end_render_pass(Default::default())
        .context("Failed to end render pass")?;

    let capture = capture_to_buffer(
        &mut builder,
        state.ctx.memory_allocator.clone(),
        state.surface.images[image_index as usize].clone(),
        state.surface.image_format(),
    )?;

    let command_buffer = builder.build().context("Failed to build command buffer")?;

    let future = state
        .previous_frame_end
        .take()
        .unwrap()
        .join(acquire_future)
        .then_execute(state.ctx.queue.clone(), command_buffer)
        .context("Failed to execute")?
        .then_signal_fence_and_flush()
        .map_err(Validated::unwrap)
        .context("Failed to flush")?;

    future.wait(None).context("Failed to wait")?;
    state.previous_frame_end = Some(sync::now(state.ctx.device.clone()).boxed());

    capture.to_image()
}

pub(crate) fn capture_screenshot_offscreen(
    app: &mut App,
    width: u32,
    height: u32,
) -> Result<image::RgbaImage> {
    let state = app.state.as_mut().unwrap();
    state
        .previous_frame_end
        .as_mut()
        .unwrap()
        .cleanup_finished();

    let offscreen = OffscreenTarget::new(
        &state.ctx,
        state.surface.render_pass.clone(),
        width,
        height,
        state.surface.image_format(),
    )?;

    let extent = offscreen.extent();
    let scale_factor = state.scale_factor;
    let (sidebar_ratio, graph_ratio) = (app.sidebar_ratio, app.graph_ratio);
    let elapsed = app.app_start.elapsed().as_secs_f32();
    let (graph_output, chrome_output, overlay_output) = build_ui_output(
        &mut app.tabs,
        app.active_tab,
        &app.tab_bar,
        &mut app.toast_manager,
        &mut app.tooltip,
        app.active_modal,
        &app.repo_dialog,
        &app.clone_dialog,
        &app.settings_dialog,
        &app.token_dialog,
        &app.confirm_dialog,
        &app.error_dialog,
        &app.branch_name_dialog,
        &app.remote_dialog,
        &app.merge_dialog,
        &app.rebase_dialog,
        &app.pull_dialog,
        &app.push_dialog,
        &state.text_renderer,
        &state.bold_text_renderer,
        scale_factor,
        extent,
        &mut state.avatar_cache,
        &state.avatar_renderer,
        &state.icon_renderer,
        sidebar_ratio,
        graph_ratio,
        app.staging_preview_ratio,
        app.shortcut_bar_visible,
        (0.0, 0.0), // No mouse interaction for offscreen screenshots
        elapsed,
    );

    let state = app.state.as_mut().unwrap();
    let viewport = Viewport {
        offset: [0.0, 0.0],
        extent: [extent[0] as f32, extent[1] as f32],
        depth_range: 0.0..=1.0,
    };

    // Upload avatar/icon atlas in a separate command buffer if dirty
    if state.avatar_renderer.needs_upload() || state.icon_renderer.needs_upload() {
        let mut upload_builder = AutoCommandBufferBuilder::primary(
            state.ctx.command_buffer_allocator.clone(),
            state.ctx.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .context("Failed to create upload command buffer")?;

        state.avatar_renderer.upload_atlas(&mut upload_builder)?;
        state.icon_renderer.upload_atlas(&mut upload_builder)?;

        let upload_cb = upload_builder
            .build()
            .context("Failed to build upload command buffer")?;
        let upload_future = state
            .previous_frame_end
            .take()
            .unwrap()
            .then_execute(state.ctx.queue.clone(), upload_cb)
            .context("Failed to execute upload")?
            .then_signal_fence_and_flush()
            .map_err(Validated::unwrap)
            .context("Failed to flush upload")?;
        upload_future
            .wait(None)
            .context("Failed to wait for upload")?;
        state.previous_frame_end = Some(sync::now(state.ctx.device.clone()).boxed());
    }

    let mut builder = AutoCommandBufferBuilder::primary(
        state.ctx.command_buffer_allocator.clone(),
        state.ctx.queue.queue_family_index(),
        CommandBufferUsage::OneTimeSubmit,
    )
    .context("Failed to create command buffer")?;

    builder
        .begin_render_pass(
            RenderPassBeginInfo {
                clear_values: vec![Some(clear_color_for_format(offscreen.format).into()), None],
                ..RenderPassBeginInfo::framebuffer(offscreen.framebuffer.clone())
            },
            Default::default(),
        )
        .context("Failed to begin render pass")?;

    render_output_to_builder(&mut builder, state, graph_output, viewport.clone())?;
    render_output_to_builder(&mut builder, state, chrome_output, viewport.clone())?;
    render_output_to_builder(&mut builder, state, overlay_output, viewport)?;

    builder
        .end_render_pass(Default::default())
        .context("Failed to end render pass")?;

    let capture = capture_to_buffer(
        &mut builder,
        state.ctx.memory_allocator.clone(),
        offscreen.image.clone(),
        offscreen.format,
    )?;

    let command_buffer = builder.build().context("Failed to build command buffer")?;

    let future = state
        .previous_frame_end
        .take()
        .unwrap()
        .then_execute(state.ctx.queue.clone(), command_buffer)
        .context("Failed to execute")?
        .then_signal_fence_and_flush()
        .map_err(Validated::unwrap)
        .context("Failed to flush")?;

    future.wait(None).context("Failed to wait")?;
    state.previous_frame_end = Some(sync::now(state.ctx.device.clone()).boxed());

    capture.to_image()
}

/// Apply a UI state for screenshot capture (e.g., showing dialogs, search bar, context menus).
pub(crate) fn apply_screenshot_state(app: &mut App) {
    let Some(ref state_str) = app.cli_args.screenshot_state else {
        return;
    };

    match state_str.as_str() {
        "open-dialog" => {
            app.repo_dialog.show();
            app.active_modal = Some(ActiveModal::RepoDialog);
        }
        "search" => {
            if let Some((_, view_state)) = app.tabs.get_mut(app.active_tab) {
                view_state.commit_graph_view.search_bar.activate();
                view_state.commit_graph_view.search_bar.set_query("example");
            }
        }
        "context-menu" => {
            let extent = app.state.as_ref().unwrap().surface.extent();
            let cx = extent[0] as f32 * 0.4;
            let cy = extent[1] as f32 * 0.3;
            if let Some((_, view_state)) = app.tabs.get_mut(app.active_tab) {
                let items = vec![
                    MenuItem::new("Copy SHA", "copy_sha"),
                    MenuItem::new("View Details", "view_details").with_shortcut("Enter"),
                    MenuItem::separator(),
                    MenuItem::new("Checkout", "checkout"),
                ];
                view_state.context_menu.show(items, cx, cy);
            }
        }
        "commit-detail" => {
            if let Some((repo_tab, view_state)) = app.tabs.get_mut(app.active_tab)
                && let Some(first) = repo_tab.commits.first()
            {
                let oid = first.id;
                let repo = &repo_tab.repo;
                if let Ok(info) = repo.full_commit_info(oid) {
                    let diff_files = repo.diff_for_commit(oid).unwrap_or_default();
                    let sm_entries = repo.submodules_at_commit(oid).unwrap_or_default();
                    view_state
                        .commit_detail_view
                        .set_commit(info, diff_files.clone(), sm_entries);
                    if let Some(first_file) = diff_files.first() {
                        let title = first_file.path.clone();
                        view_state
                            .diff_view
                            .set_diff(vec![first_file.clone()], title);
                    }
                }
            }
        }
        "confirm-dialog" => {
            app.confirm_dialog.show(
                "Delete Branch",
                "Delete branch 'feature'? This cannot be undone.",
            );
            app.active_modal = Some(ActiveModal::Confirm);
        }
        "merge-dialog" => {
            app.merge_dialog.show("feature", "main", 2);
            app.active_modal = Some(ActiveModal::Merge);
        }
        "rebase-dialog" => {
            app.rebase_dialog.show("main", "feature", 1);
            app.active_modal = Some(ActiveModal::Rebase);
        }
        "pull-dialog" => {
            app.pull_dialog.show("main", "origin");
            app.active_modal = Some(ActiveModal::Pull);
        }
        "push-dialog" => {
            app.push_dialog.show("main", "origin");
            app.active_modal = Some(ActiveModal::Push);
        }
        "settings-dialog" => {
            app.settings_dialog.show();
            app.active_modal = Some(ActiveModal::Settings);
        }
        other => {
            eprintln!(
                "Unknown screenshot state: '{}'. Valid states: open-dialog, search, context-menu, commit-detail, confirm-dialog, merge-dialog, rebase-dialog, pull-dialog, push-dialog, settings-dialog",
                other
            );
        }
    }
}
