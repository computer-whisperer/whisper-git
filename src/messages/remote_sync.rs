use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use winit::event_loop::EventLoopProxy;

use crate::git::{self, GitRepo, RemoteOpResult};
use crate::ui::widgets::{ToastManager, ToastSeverity};

use super::{AppMessage, MessageViewState};

/// Start a remote operation (fetch/pull/push) with common boilerplate:
/// check that no operation is already in progress on the given receiver,
/// verify a working directory exists, then launch the async function and
/// store the receiver. Returns `false` if the operation was already busy.
#[allow(clippy::too_many_arguments)]
fn start_remote_op(
    receiver: &mut Option<(Receiver<RemoteOpResult>, std::time::Instant, String)>,
    repo: &GitRepo,
    op_name: &str,
    remote_name: String,
    start_fn: impl FnOnce(PathBuf) -> Receiver<RemoteOpResult>,
    set_header_flag: impl FnOnce(&mut crate::ui::widgets::HeaderBar),
    toast_manager: &mut ToastManager,
    header_bar: &mut crate::ui::widgets::HeaderBar,
) -> bool {
    if receiver.is_some() {
        toast_manager.push(
            format!("{} already in progress", op_name),
            ToastSeverity::Info,
        );
        return false;
    }
    if !repo.has_remotes() {
        toast_manager.push(
            "No remotes configured. Add one via the REMOTE section in the sidebar.",
            ToastSeverity::Error,
        );
        return false;
    }
    let cmd_dir = repo.git_command_dir();
    let rx = start_fn(cmd_dir);
    *receiver = Some((rx, std::time::Instant::now(), remote_name));
    set_header_flag(header_bar);
    true
}

/// Handle fetch/pull/push and related remote-sync messages.
/// Returns `Some(handled)` when the message belongs to this domain.
pub(super) fn handle_remote_sync_message(
    msg: &AppMessage,
    repo: &GitRepo,
    view_state: &mut MessageViewState<'_>,
    toast_manager: &mut ToastManager,
    proxy: &EventLoopProxy<()>,
) -> Option<bool> {
    match msg {
        AppMessage::Fetch(remote_name) => {
            let remote = remote_name.clone().unwrap_or_else(|| {
                repo.default_remote()
                    .unwrap_or_else(|_| "origin".to_string())
            });
            // Auto-fix missing fetch refspec (common with bare-cloned repos)
            if repo.remote_missing_fetch_refspec(&remote) {
                match repo.add_default_fetch_refspec(&remote) {
                    Ok(()) => {
                        toast_manager.push(
                            format!("Configured fetch tracking for '{}'", remote),
                            ToastSeverity::Info,
                        );
                    }
                    Err(e) => {
                        toast_manager.push(
                            format!(
                                "Cannot fetch from '{}': it has no tracking configuration and auto-fix failed ({})",
                                remote, e
                            ),
                            ToastSeverity::Error,
                        );
                        return Some(false);
                    }
                }
            }
            let remote_for_closure = remote.clone();
            let p = proxy.clone();
            if !start_remote_op(
                view_state.fetch_receiver,
                repo,
                "Fetch",
                remote,
                |wd| git::fetch_remote_async(wd, remote_for_closure, p),
                |hb| hb.fetching = true,
                toast_manager,
                view_state.header_bar,
            ) {
                return Some(false);
            }
            Some(true)
        }
        AppMessage::FetchAll => {
            // Auto-fix missing fetch refspecs on all remotes
            for name in repo.remote_names() {
                if repo.remote_missing_fetch_refspec(&name) {
                    let _ = repo.add_default_fetch_refspec(&name);
                }
            }
            let p = proxy.clone();
            if !start_remote_op(
                view_state.fetch_receiver,
                repo,
                "Fetch All",
                "all remotes".to_string(),
                |wd| git::fetch_all_async(wd, p),
                |hb| hb.fetching = true,
                toast_manager,
                view_state.header_bar,
            ) {
                return Some(false);
            }
            Some(true)
        }
        AppMessage::Pull {
            remote: remote_name,
            branch,
        } => {
            let remote = remote_name.clone().unwrap_or_else(|| {
                repo.default_remote()
                    .unwrap_or_else(|_| "origin".to_string())
            });
            let remote_for_closure = remote.clone();
            let branch_for_closure = branch.clone();
            let p = proxy.clone();
            if !start_remote_op(
                view_state.pull_receiver,
                repo,
                "Pull",
                remote,
                |wd| git::pull_remote_async(wd, remote_for_closure, branch_for_closure, p),
                |hb| hb.pulling = true,
                toast_manager,
                view_state.header_bar,
            ) {
                return Some(false);
            }
            Some(true)
        }
        AppMessage::PullRebase {
            remote: remote_name,
            branch,
        } => {
            let remote = remote_name.clone().unwrap_or_else(|| {
                repo.default_remote()
                    .unwrap_or_else(|_| "origin".to_string())
            });
            let remote_for_closure = remote.clone();
            let branch_for_closure = branch.clone();
            let p = proxy.clone();
            if !start_remote_op(
                view_state.pull_receiver,
                repo,
                "Pull",
                remote,
                |wd| git::pull_rebase_async(wd, remote_for_closure, branch_for_closure, p),
                |hb| hb.pulling = true,
                toast_manager,
                view_state.header_bar,
            ) {
                return Some(false);
            }
            Some(true)
        }
        AppMessage::ShowPullDialog(_) => Some(true), // handled in main.rs
        AppMessage::PullBranchFrom {
            remote,
            branch,
            rebase,
        } => {
            let remote_for_closure = remote.clone();
            let branch_for_closure = branch.clone();
            let p = proxy.clone();
            if !start_remote_op(
                view_state.pull_receiver,
                repo,
                "Pull",
                remote.clone(),
                |wd| {
                    if *rebase {
                        git::pull_rebase_async(wd, remote_for_closure, branch_for_closure, p)
                    } else {
                        git::pull_remote_async(wd, remote_for_closure, branch_for_closure, p)
                    }
                },
                |hb| hb.pulling = true,
                toast_manager,
                view_state.header_bar,
            ) {
                return Some(false);
            }
            Some(true)
        }
        AppMessage::Push {
            remote: remote_name,
            branch,
        } => {
            let remote = remote_name.clone().unwrap_or_else(|| {
                repo.default_remote()
                    .unwrap_or_else(|_| "origin".to_string())
            });
            let remote_for_closure = remote.clone();
            let branch_for_closure = branch.clone();
            let p = proxy.clone();
            if !start_remote_op(
                view_state.push_receiver,
                repo,
                "Push",
                remote,
                |wd| git::push_remote_async(wd, remote_for_closure, branch_for_closure, p),
                |hb| hb.pushing = true,
                toast_manager,
                view_state.header_bar,
            ) {
                return Some(false);
            }
            Some(true)
        }
        AppMessage::PushForce {
            remote: remote_name,
            branch,
        } => {
            let remote = remote_name.clone().unwrap_or_else(|| {
                repo.default_remote()
                    .unwrap_or_else(|_| "origin".to_string())
            });
            let remote_for_closure = remote.clone();
            let branch_for_closure = branch.clone();
            let p = proxy.clone();
            if !start_remote_op(
                view_state.push_receiver,
                repo,
                "Push",
                remote,
                |wd| git::push_force_async(wd, remote_for_closure, branch_for_closure, p),
                |hb| hb.pushing = true,
                toast_manager,
                view_state.header_bar,
            ) {
                return Some(false);
            }
            toast_manager.push("Force pushing...", ToastSeverity::Info);
            Some(true)
        }
        AppMessage::ShowPushDialog(_) => Some(true), // handled in main.rs
        AppMessage::PushBranchTo {
            local_branch,
            remote,
            remote_branch,
            force,
        } => {
            let refspec = format!("{}:{}", local_branch, remote_branch);
            let remote_for_closure = remote.clone();
            let refspec_for_closure = refspec.clone();
            let toast_msg = if *force {
                format!(
                    "Force pushing {} to {}/{}...",
                    local_branch, remote, remote_branch
                )
            } else {
                format!(
                    "Pushing {} to {}/{}...",
                    local_branch, remote, remote_branch
                )
            };
            if *force {
                let p = proxy.clone();
                if !start_remote_op(
                    view_state.push_receiver,
                    repo,
                    "Push",
                    remote.clone(),
                    |wd| {
                        git::push_force_refspec_async(
                            wd,
                            remote_for_closure,
                            refspec_for_closure,
                            p,
                        )
                    },
                    |hb| hb.pushing = true,
                    toast_manager,
                    view_state.header_bar,
                ) {
                    return Some(false);
                }
                toast_manager.push(toast_msg, ToastSeverity::Info);
            } else {
                let p = proxy.clone();
                if !start_remote_op(
                    view_state.push_receiver,
                    repo,
                    "Push",
                    remote.clone(),
                    |wd| git::push_refspec_async(wd, remote_for_closure, refspec_for_closure, p),
                    |hb| hb.pushing = true,
                    toast_manager,
                    view_state.header_bar,
                ) {
                    return Some(false);
                }
                toast_manager.push(toast_msg, ToastSeverity::Info);
            }
            Some(true)
        }
        _ => None,
    }
}
