use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use git2::Oid;

use crate::git::{self, CommitInfo, DiffFile, GitRepo, RemoteOpResult};
use crate::ui::Rect;
use crate::ui::widgets::{ToastManager, ToastSeverity};
use crate::views::{BranchSidebar, CommitDetailView, CommitGraphView, DiffView, StagingWell};

/// What content mode the right panel is showing
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RightPanelMode {
    /// Default: file lists + commit message + buttons (upper), selected file diff (lower)
    #[default]
    Staging,
    /// Shown when a commit is selected in graph: commit detail (upper), file diff (lower)
    Browse,
}

/// Maximum number of commits to load into the graph view.
const MAX_COMMITS: usize = 50;

/// Application-level messages for state changes
#[derive(Clone, Debug)]
pub enum AppMessage {
    StageFile(String),
    UnstageFile(String),
    StageAll,
    UnstageAll,
    Commit(String),
    Fetch,
    Pull,
    PullRebase,
    Push,
    PushForce,
    SelectedCommit(Oid),
    ViewCommitFileDiff(Oid, String),
    ViewDiff(String, bool), // (path, staged)
    CheckoutBranch(String),
    CheckoutRemoteBranch(String, String),
    DeleteBranch(String),
    StageHunk(String, usize),    // (file_path, hunk_index)
    UnstageHunk(String, usize),  // (file_path, hunk_index)
    DiscardFile(String),
    LoadMoreCommits,
    DeleteSubmodule(String),
    UpdateSubmodule(String),
    JumpToWorktreeBranch(String),
    RemoveWorktree(String),
    MergeBranch(String),
    RebaseBranch(String),
    CreateBranch(String, Oid),  // (name, at_commit)
    CreateTag(String, Oid),     // (name, at_commit)
    DeleteTag(String),
    StashPush,
    StashPop,
    StashApply(usize),
    StashDrop(usize),
    StashPopIndex(usize),
    CherryPick(Oid),
    AmendCommit(String),
    ToggleAmend,
    RevertCommit(Oid),
    ResetToCommit(Oid, git2::ResetType),
    EnterSubmodule(String),
    ExitSubmodule,
    ExitToDepth(usize),
    AbortOperation,
    AddRemote(String, String),     // (name, url)
    DeleteRemote(String),
    RenameRemote(String, String),  // (old_name, new_name)
    SetRemoteUrl(String, String),  // (name, new_url)
}

/// Try to set the generic async operation receiver. Returns `true` if the
/// operation was successfully queued, or `false` if another operation is
/// already in progress (in which case a toast is shown).
pub fn queue_async_op(
    generic_op_receiver: &mut Option<(Receiver<RemoteOpResult>, String, std::time::Instant)>,
    rx: Receiver<RemoteOpResult>,
    label: String,
    in_progress_msg: String,
    toast_manager: &mut ToastManager,
) -> bool {
    if generic_op_receiver.is_some() {
        toast_manager.push("Another operation is in progress".to_string(), ToastSeverity::Info);
        return false;
    }
    *generic_op_receiver = Some((rx, label, std::time::Instant::now()));
    toast_manager.push(in_progress_msg, ToastSeverity::Info);
    true
}

/// Context needed by [`handle_app_message`] that lives outside the per-tab
/// state. The caller constructs this from `App` fields before entering the
/// message loop.
pub struct MessageContext {
    /// Graph bounds for scroll-to-selection (JumpToWorktreeBranch).
    /// Compute this from the current layout before calling the handler.
    pub graph_bounds: Rect,
}

/// Check commit preconditions common to both Commit and AmendCommit:
/// non-empty message and valid git user config. Returns `true` if all
/// preconditions pass; `false` if a precondition failed (with a toast shown).
fn validate_commit_preconditions(
    message: &str,
    staging_repo: &GitRepo,
    toast_manager: &mut ToastManager,
) -> bool {
    if message.trim().is_empty() {
        toast_manager.push(
            "Commit message cannot be empty".to_string(),
            ToastSeverity::Error,
        );
        return false;
    }
    if !staging_repo.has_user_config() {
        toast_manager.push(
            "Git user not configured. Run: git config user.name \"Your Name\" && git config user.email \"you@example.com\"".to_string(),
            ToastSeverity::Error,
        );
        return false;
    }
    true
}

/// Execute a synchronous git operation that mutates repo state, then refresh
/// the UI. On success, shows `success_msg` as a toast. On error, shows
/// `error_msg_prefix: <error>`.
fn handle_repo_mutation(
    result: Result<(), anyhow::Error>,
    success_msg: String,
    error_msg_prefix: &str,
    repo: &GitRepo,
    commits: &mut Vec<CommitInfo>,
    view_state: &mut MessageViewState<'_>,
    toast_manager: &mut ToastManager,
) {
    match result {
        Ok(()) => {
            refresh_repo_state(repo, commits, view_state, toast_manager);
            toast_manager.push(success_msg, ToastSeverity::Success);
        }
        Err(e) => {
            toast_manager.push(
                format!("{}: {}", error_msg_prefix, e),
                ToastSeverity::Error,
            );
        }
    }
}

/// Start a remote operation (fetch/pull/push) with common boilerplate:
/// check that no operation is already in progress on the given receiver,
/// verify a working directory exists, then launch the async function and
/// store the receiver. Returns `false` if the operation was already busy.
fn start_remote_op(
    receiver: &mut Option<(Receiver<RemoteOpResult>, std::time::Instant)>,
    repo: &GitRepo,
    op_name: &str,
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
    if let Some(workdir) = repo.working_dir_path() {
        let rx = start_fn(workdir);
        *receiver = Some((rx, std::time::Instant::now()));
        set_header_flag(header_bar);
        true
    } else {
        toast_manager.push(
            format!("Cannot {}: no working directory", op_name.to_lowercase()),
            ToastSeverity::Error,
        );
        false
    }
}

/// Dispatch a single `AppMessage`.
///
/// Returns `true` if the message was handled (even if it resulted in an
/// error toast). Returns `false` only when a prerequisite was not met and
/// the message was silently skipped (e.g. generic_op_receiver busy).
pub fn handle_app_message(
    msg: AppMessage,
    repo: &GitRepo,
    staging_repo: &GitRepo,
    commits: &mut Vec<CommitInfo>,
    view_state: &mut MessageViewState<'_>,
    toast_manager: &mut ToastManager,
    ctx: &MessageContext,
) -> bool {
    match msg {
        AppMessage::StageFile(path) => {
            if let Err(e) = staging_repo.stage_file(&path) {
                toast_manager.push(
                    format!("Stage failed: {}", e),
                    ToastSeverity::Error,
                );
            }
        }
        AppMessage::UnstageFile(path) => {
            if let Err(e) = staging_repo.unstage_file(&path) {
                toast_manager.push(
                    format!("Unstage failed: {}", e),
                    ToastSeverity::Error,
                );
            }
        }
        AppMessage::StageAll => {
            if let Ok(status) = staging_repo.status() {
                let total = status.unstaged.len();
                let mut failed = 0;
                for file in &status.unstaged {
                    if staging_repo.stage_file(&file.path).is_err() {
                        failed += 1;
                    }
                }
                if failed > 0 {
                    toast_manager.push(
                        format!("Staged {}/{} files ({} failed)", total - failed, total, failed),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        AppMessage::UnstageAll => {
            if let Ok(status) = staging_repo.status() {
                let total = status.staged.len();
                let mut failed = 0;
                for file in &status.staged {
                    if staging_repo.unstage_file(&file.path).is_err() {
                        failed += 1;
                    }
                }
                if failed > 0 {
                    toast_manager.push(
                        format!("Unstaged {}/{} files ({} failed)", total - failed, total, failed),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        AppMessage::Commit(message) => {
            if !validate_commit_preconditions(&message, staging_repo, toast_manager) {
                return true;
            }
            match staging_repo.commit(&message) {
                Ok(oid) => {
                    refresh_repo_state(repo, commits, view_state, toast_manager);
                    view_state.staging_well.clear_and_focus();
                    toast_manager.push(
                        format!("Commit {}", &oid.to_string()[..7]),
                        ToastSeverity::Success,
                    );
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Commit failed: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        AppMessage::Fetch => {
            let remote = repo.default_remote().unwrap_or_else(|_| "origin".to_string());
            if !start_remote_op(
                view_state.fetch_receiver, repo, "Fetch",
                |wd| git::fetch_remote_async(wd, remote),
                |hb| hb.fetching = true,
                toast_manager, view_state.header_bar,
            ) {
                return false;
            }
        }
        AppMessage::Pull => {
            let remote = repo.default_remote().unwrap_or_else(|_| "origin".to_string());
            let branch = repo.current_branch().unwrap_or_else(|_| "HEAD".to_string());
            if !start_remote_op(
                view_state.pull_receiver, repo, "Pull",
                |wd| git::pull_remote_async(wd, remote, branch),
                |hb| hb.pulling = true,
                toast_manager, view_state.header_bar,
            ) {
                return false;
            }
        }
        AppMessage::PullRebase => {
            let remote = repo.default_remote().unwrap_or_else(|_| "origin".to_string());
            let branch = repo.current_branch().unwrap_or_else(|_| "HEAD".to_string());
            if !start_remote_op(
                view_state.pull_receiver, repo, "Pull",
                |wd| git::pull_rebase_async(wd, remote, branch),
                |hb| hb.pulling = true,
                toast_manager, view_state.header_bar,
            ) {
                return false;
            }
        }
        AppMessage::Push => {
            let remote = repo.default_remote().unwrap_or_else(|_| "origin".to_string());
            let branch = repo.current_branch().unwrap_or_else(|_| "HEAD".to_string());
            if !start_remote_op(
                view_state.push_receiver, repo, "Push",
                |wd| git::push_remote_async(wd, remote, branch),
                |hb| hb.pushing = true,
                toast_manager, view_state.header_bar,
            ) {
                return false;
            }
        }
        AppMessage::PushForce => {
            let remote = repo.default_remote().unwrap_or_else(|_| "origin".to_string());
            let branch = repo.current_branch().unwrap_or_else(|_| "HEAD".to_string());
            if !start_remote_op(
                view_state.push_receiver, repo, "Push",
                |wd| git::push_force_async(wd, remote, branch),
                |hb| hb.pushing = true,
                toast_manager, view_state.header_bar,
            ) {
                return false;
            }
            toast_manager.push("Force pushing...", ToastSeverity::Info);
        }
        AppMessage::SelectedCommit(oid) => {
            let full_info = repo.full_commit_info(oid);
            match repo.diff_for_commit(oid) {
                Ok(diff_files) => {
                    if let Ok(info) = full_info {
                        view_state.commit_detail_view.set_commit(info, diff_files.clone());
                    }
                    if let Some(first_file) = diff_files.first() {
                        let title = first_file.path.clone();
                        view_state.diff_view.set_diff(vec![first_file.clone()], title);
                    } else {
                        let title = commits.iter()
                            .find(|c| c.id == oid)
                            .map(|c| format!("{} {}", c.short_id, c.summary))
                            .unwrap_or_else(|| oid.to_string());
                        view_state.diff_view.set_diff(diff_files, title);
                    }
                    *view_state.last_diff_commit = Some(oid);
                    *view_state.right_panel_mode = RightPanelMode::Browse;
                }
                Err(e) => {
                    toast_manager.push(format!("Failed to load diff: {}", e), ToastSeverity::Error);
                }
            }
        }
        AppMessage::ViewCommitFileDiff(oid, path) => {
            match repo.diff_file_in_commit(oid, &path) {
                Ok(diff_files) => {
                    view_state.diff_view.set_diff(diff_files, path);
                }
                Err(e) => {
                    toast_manager.push(format!("Failed to load diff: {}", e), ToastSeverity::Error);
                }
            }
        }
        AppMessage::ViewDiff(path, staged) => {
            match staging_repo.diff_working_file(&path, staged) {
                Ok(hunks) => {
                    let diff_file = DiffFile::from_hunks(path.clone(), hunks);
                    let title = if staged {
                        format!("Staged: {}", path)
                    } else {
                        format!("Unstaged: {}", path)
                    };
                    if staged {
                        view_state.diff_view.set_staged_diff(vec![diff_file], title);
                    } else {
                        view_state.diff_view.set_diff(vec![diff_file], title);
                    }
                    *view_state.last_diff_commit = None;
                }
                Err(e) => {
                    toast_manager.push(format!("Failed to load diff: {}", e), ToastSeverity::Error);
                }
            }
        }
        AppMessage::CheckoutBranch(name) => {
            handle_repo_mutation(
                repo.checkout_branch(&name),
                format!("Switched to {}", name),
                "Checkout failed",
                repo, commits, view_state, toast_manager,
            );
        }
        AppMessage::CheckoutRemoteBranch(remote, branch) => {
            handle_repo_mutation(
                repo.checkout_remote_branch(&remote, &branch),
                format!("Switched to {}/{}", remote, branch),
                "Checkout failed",
                repo, commits, view_state, toast_manager,
            );
        }
        AppMessage::DeleteBranch(name) => {
            match repo.delete_branch(&name) {
                Ok(()) => {
                    refresh_repo_state(repo, commits, view_state, toast_manager);
                    toast_manager.push(
                        format!("Deleted branch {}", name),
                        ToastSeverity::Success,
                    );
                }
                Err(e) => {
                    // Show root cause for a cleaner message
                    let root = e.root_cause().to_string();
                    toast_manager.push(
                        format!("Cannot delete '{}': {}", name, root),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        AppMessage::StageHunk(path, hunk_idx) => {
            match staging_repo.stage_hunk(&path, hunk_idx) {
                Ok(()) => {
                    toast_manager.push(
                        format!("Staged hunk {} in {}", hunk_idx + 1, path),
                        ToastSeverity::Success,
                    );
                    if let Ok(hunks) = staging_repo.diff_working_file(&path, false) {
                        if hunks.is_empty() {
                            view_state.diff_view.clear();
                        } else {
                            let diff_file = DiffFile::from_hunks(path.clone(), hunks);
                            view_state.diff_view.set_diff(vec![diff_file], path);
                        }
                    }
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Stage hunk failed: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        AppMessage::UnstageHunk(path, hunk_idx) => {
            match staging_repo.unstage_hunk(&path, hunk_idx) {
                Ok(()) => {
                    toast_manager.push(
                        format!("Unstaged hunk {} in {}", hunk_idx + 1, path),
                        ToastSeverity::Success,
                    );
                    if let Ok(hunks) = staging_repo.diff_working_file(&path, true) {
                        if hunks.is_empty() {
                            view_state.diff_view.clear();
                        } else {
                            let diff_file = DiffFile::from_hunks(path.clone(), hunks);
                            view_state.diff_view.set_staged_diff(vec![diff_file], path);
                        }
                    }
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Unstage hunk failed: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        AppMessage::DiscardFile(path) => {
            match staging_repo.discard_file(&path) {
                Ok(()) => {
                    toast_manager.push(
                        format!("Discarded: {}", path),
                        ToastSeverity::Info,
                    );
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Discard failed: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        AppMessage::LoadMoreCommits => {
            // Count only real commits (exclude synthetics) for the load-more request
            let real_count = commits.iter().filter(|c| !c.is_synthetic).count();
            let new_count = real_count + 50;
            if let Ok(new_commits) = repo.commit_graph(new_count) {
                *commits = new_commits;
                // Re-add synthetic entries
                let worktrees = repo.worktrees().unwrap_or_default();
                let mut synthetics = git::create_synthetic_entries(repo, &worktrees, commits);
                if !synthetics.is_empty() {
                    synthetics.append(commits);
                    *commits = synthetics;
                }
                view_state.commit_graph_view.update_layout(commits);
            }
            view_state.commit_graph_view.finish_loading();
        }
        AppMessage::DeleteSubmodule(name) => {
            if let Some(workdir) = repo.working_dir_path() {
                let rx = git::remove_submodule_async(workdir, name.clone());
                queue_async_op(
                    view_state.generic_op_receiver,
                    rx,
                    format!("Delete submodule '{}'", name),
                    format!("Removing submodule '{}'...", name),
                    toast_manager,
                );
            }
        }
        AppMessage::UpdateSubmodule(name) => {
            if let Some(workdir) = repo.working_dir_path() {
                let rx = git::update_submodule_async(workdir, name.clone());
                queue_async_op(
                    view_state.generic_op_receiver,
                    rx,
                    format!("Update submodule '{}'", name),
                    format!("Updating submodule '{}'...", name),
                    toast_manager,
                );
            }
        }
        AppMessage::JumpToWorktreeBranch(name) => {
            // Find the worktree by name, get its branch, find the branch tip, select it
            if let Some(wt) = view_state.branch_sidebar.worktrees.iter().find(|w| w.name == name) {
                let branch_name = wt.branch.clone();
                if let Some(tip) = view_state.commit_graph_view.branch_tips.iter()
                    .find(|t| t.name == branch_name && !t.is_remote) {
                        view_state.commit_graph_view.selected_commit = Some(tip.oid);
                        view_state.commit_graph_view.scroll_to_selection(commits, ctx.graph_bounds);
                        toast_manager.push(format!("Jumped to branch '{}'", branch_name), ToastSeverity::Info);
                } else {
                    toast_manager.push(format!("Branch '{}' not found in graph", branch_name), ToastSeverity::Error);
                }
            } else {
                toast_manager.push(format!("Worktree '{}' not found", name), ToastSeverity::Error);
            }
        }
        AppMessage::RemoveWorktree(name) => {
            if let Some(workdir) = repo.working_dir_path() {
                let rx = git::remove_worktree_async(workdir, name.clone());
                queue_async_op(
                    view_state.generic_op_receiver,
                    rx,
                    format!("Remove worktree '{}'", name),
                    format!("Removing worktree '{}'...", name),
                    toast_manager,
                );
            }
        }
        AppMessage::MergeBranch(name) => {
            if let Some(workdir) = repo.working_dir_path() {
                let rx = git::merge_branch_async(workdir, name.clone());
                queue_async_op(
                    view_state.generic_op_receiver,
                    rx,
                    format!("Merge '{}'", name),
                    format!("Merging '{}'...", name),
                    toast_manager,
                );
            }
        }
        AppMessage::RebaseBranch(name) => {
            if let Some(workdir) = repo.working_dir_path() {
                let rx = git::rebase_branch_async(workdir, name.clone());
                queue_async_op(
                    view_state.generic_op_receiver,
                    rx,
                    format!("Rebase onto '{}'", name),
                    format!("Rebasing onto '{}'...", name),
                    toast_manager,
                );
            }
        }
        AppMessage::CreateBranch(name, oid) => {
            handle_repo_mutation(
                repo.create_branch_at(&name, oid),
                format!("Created branch '{}'", name),
                "Create branch failed",
                repo, commits, view_state, toast_manager,
            );
        }
        AppMessage::CreateTag(name, oid) => {
            handle_repo_mutation(
                repo.create_tag(&name, oid),
                format!("Created tag '{}'", name),
                "Create tag failed",
                repo, commits, view_state, toast_manager,
            );
        }
        AppMessage::DeleteTag(name) => {
            handle_repo_mutation(
                repo.delete_tag(&name),
                format!("Deleted tag '{}'", name),
                "Delete tag failed",
                repo, commits, view_state, toast_manager,
            );
        }
        AppMessage::StashPush => {
            if let Some(workdir) = repo.working_dir_path() {
                let rx = git::stash_push_async(workdir);
                queue_async_op(
                    view_state.generic_op_receiver,
                    rx,
                    "Stash push".to_string(),
                    "Stashing changes...".to_string(),
                    toast_manager,
                );
            }
        }
        AppMessage::StashPop => {
            if let Some(workdir) = repo.working_dir_path() {
                let rx = git::stash_pop_async(workdir);
                queue_async_op(
                    view_state.generic_op_receiver,
                    rx,
                    "Stash pop".to_string(),
                    "Popping stash...".to_string(),
                    toast_manager,
                );
            }
        }
        AppMessage::StashApply(index) => {
            if let Some(workdir) = repo.working_dir_path() {
                let rx = git::stash_apply_async(workdir, index);
                queue_async_op(
                    view_state.generic_op_receiver,
                    rx,
                    format!("Stash apply @{{{}}}", index),
                    format!("Applying stash@{{{}}}...", index),
                    toast_manager,
                );
            }
        }
        AppMessage::StashDrop(index) => {
            if let Some(workdir) = repo.working_dir_path() {
                let rx = git::stash_drop_async(workdir, index);
                queue_async_op(
                    view_state.generic_op_receiver,
                    rx,
                    format!("Stash drop @{{{}}}", index),
                    format!("Dropping stash@{{{}}}...", index),
                    toast_manager,
                );
            }
        }
        AppMessage::StashPopIndex(index) => {
            if let Some(workdir) = repo.working_dir_path() {
                let rx = git::stash_pop_index_async(workdir, index);
                queue_async_op(
                    view_state.generic_op_receiver,
                    rx,
                    format!("Stash pop @{{{}}}", index),
                    format!("Popping stash@{{{}}}...", index),
                    toast_manager,
                );
            }
        }
        AppMessage::CherryPick(oid) => {
            if let Some(workdir) = repo.working_dir_path() {
                let sha = oid.to_string();
                let rx = git::cherry_pick_async(workdir, sha.clone());
                queue_async_op(
                    view_state.generic_op_receiver,
                    rx,
                    format!("Cherry-pick {}", &sha[..7]),
                    format!("Cherry-picking {}...", &sha[..7]),
                    toast_manager,
                );
            }
        }
        AppMessage::AmendCommit(message) => {
            if !validate_commit_preconditions(&message, staging_repo, toast_manager) {
                return true;
            }
            match staging_repo.amend_commit(&message) {
                Ok(oid) => {
                    refresh_repo_state(repo, commits, view_state, toast_manager);
                    view_state.staging_well.exit_amend_mode();
                    toast_manager.push(
                        format!("Amended {}", &oid.to_string()[..7]),
                        ToastSeverity::Success,
                    );
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Amend failed: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        AppMessage::ToggleAmend => {
            if view_state.staging_well.amend_mode {
                view_state.staging_well.exit_amend_mode();
            } else if let Some((subject, body)) = staging_repo.head_commit_message() {
                view_state.staging_well.enter_amend_mode(&subject, &body);
            } else {
                toast_manager.push(
                    "No HEAD commit to amend".to_string(),
                    ToastSeverity::Error,
                );
            }
        }
        AppMessage::RevertCommit(oid) => {
            if let Some(workdir) = repo.working_dir_path() {
                let sha = oid.to_string();
                let rx = git::revert_commit_async(workdir, sha.clone());
                queue_async_op(
                    view_state.generic_op_receiver,
                    rx,
                    format!("Revert {}", &sha[..7]),
                    format!("Reverting {}...", &sha[..7]),
                    toast_manager,
                );
            }
        }
        AppMessage::ResetToCommit(oid, mode) => {
            let mode_name = match mode {
                git2::ResetType::Soft => "soft",
                git2::ResetType::Mixed => "mixed",
                git2::ResetType::Hard => "hard",
            };
            handle_repo_mutation(
                repo.reset_to_commit(oid, mode),
                format!("Reset ({}) to {}", mode_name, &oid.to_string()[..7]),
                "Reset failed",
                repo, commits, view_state, toast_manager,
            );
        }

        AppMessage::AbortOperation => {
            handle_repo_mutation(
                repo.cleanup_state(),
                "Operation aborted".to_string(),
                "Abort failed",
                repo, commits, view_state, toast_manager,
            );
        }

        AppMessage::AddRemote(name, url) => {
            handle_repo_mutation(
                repo.add_remote(&name, &url),
                format!("Added remote '{}'", name),
                "Failed to add remote",
                repo, commits, view_state, toast_manager,
            );
        }

        AppMessage::DeleteRemote(name) => {
            handle_repo_mutation(
                repo.delete_remote(&name),
                format!("Deleted remote '{}'", name),
                "Failed to delete remote",
                repo, commits, view_state, toast_manager,
            );
        }

        AppMessage::RenameRemote(old_name, new_name) => {
            handle_repo_mutation(
                repo.rename_remote(&old_name, &new_name),
                format!("Renamed remote '{}' to '{}'", old_name, new_name),
                "Failed to rename remote",
                repo, commits, view_state, toast_manager,
            );
        }

        AppMessage::SetRemoteUrl(name, url) => {
            handle_repo_mutation(
                repo.set_remote_url(&name, &url),
                format!("Updated URL for '{}'", name),
                "Failed to update remote URL",
                repo, commits, view_state, toast_manager,
            );
        }

        // Submodule navigation messages are handled in main.rs process_messages,
        // not here. If they leak through, just ignore them.
        AppMessage::EnterSubmodule(_) | AppMessage::ExitSubmodule | AppMessage::ExitToDepth(_) => {
            return false;
        }
    }

    true
}

/// A borrowing view into `TabViewState` fields needed by the message handler.
///
/// This avoids passing the entire `TabViewState` (which contains fields
/// unrelated to message handling) and makes the required dependencies
/// explicit.
pub struct MessageViewState<'a> {
    pub commit_graph_view: &'a mut CommitGraphView,
    pub staging_well: &'a mut StagingWell,
    pub diff_view: &'a mut DiffView,
    pub commit_detail_view: &'a mut CommitDetailView,
    pub branch_sidebar: &'a mut BranchSidebar,
    pub header_bar: &'a mut crate::ui::widgets::HeaderBar,
    pub submodule_strip: &'a mut crate::ui::widgets::SubmoduleStatusStrip,
    pub last_diff_commit: &'a mut Option<Oid>,
    pub fetch_receiver: &'a mut Option<(Receiver<RemoteOpResult>, std::time::Instant)>,
    pub pull_receiver: &'a mut Option<(Receiver<RemoteOpResult>, std::time::Instant)>,
    pub push_receiver: &'a mut Option<(Receiver<RemoteOpResult>, std::time::Instant)>,
    pub generic_op_receiver: &'a mut Option<(Receiver<RemoteOpResult>, String, std::time::Instant)>,
    pub right_panel_mode: &'a mut RightPanelMode,
}

/// Refresh commits, branch tips, tags, and header info from the repo.
/// Call this after any operation that changes branches, commits, or remote state.
fn refresh_repo_state(
    repo: &GitRepo,
    commits: &mut Vec<CommitInfo>,
    view_state: &mut MessageViewState<'_>,
    toast_manager: &mut ToastManager,
) {
    match repo.commit_graph(MAX_COMMITS) {
        Ok(c) => *commits = c,
        Err(e) => {
            toast_manager.push(
                format!("Failed to load commits: {}", e),
                ToastSeverity::Error,
            );
            *commits = Vec::new();
        }
    }
    view_state.commit_graph_view.head_oid = repo.head_oid().ok();

    let branch_tips = repo.branch_tips().unwrap_or_else(|e| {
        toast_manager.push(format!("Failed to load branches: {}", e), ToastSeverity::Error);
        Vec::new()
    });
    let tags = repo.tags().unwrap_or_else(|e| {
        toast_manager.push(format!("Failed to load tags: {}", e), ToastSeverity::Error);
        Vec::new()
    });
    let current = repo.current_branch().unwrap_or_else(|e| {
        toast_manager.push(format!("Failed to get current branch: {}", e), ToastSeverity::Error);
        String::new()
    });

    let worktrees = repo.worktrees().unwrap_or_else(|e| {
        toast_manager.push(format!("Failed to load worktrees: {}", e), ToastSeverity::Error);
        Vec::new()
    });

    // Prepend synthetic "uncommitted changes" entries for dirty worktrees
    let mut synthetics = git::create_synthetic_entries(repo, &worktrees, commits);
    if !synthetics.is_empty() {
        synthetics.append(commits);
        *commits = synthetics;
    }

    view_state.commit_graph_view.update_layout(commits);
    view_state.commit_graph_view.branch_tips = branch_tips.clone();
    view_state.commit_graph_view.tags = tags.clone();
    view_state.commit_graph_view.worktrees = worktrees.clone();
    view_state.branch_sidebar.set_branch_data(&branch_tips, &tags, current.clone());
    let current_workdir = repo.workdir().unwrap_or(std::path::Path::new(""));
    view_state.staging_well.set_worktrees(&worktrees, current_workdir);
    view_state.branch_sidebar.worktrees = worktrees;

    let submodules = repo.submodules().unwrap_or_else(|e| {
        toast_manager.push(format!("Failed to load submodules: {}", e), ToastSeverity::Error);
        Vec::new()
    });
    view_state.submodule_strip.submodules = submodules.clone();
    view_state.branch_sidebar.submodules = submodules;

    let (ahead, behind) = repo.ahead_behind().unwrap_or_else(|e| {
        toast_manager.push(format!("Failed to compute ahead/behind: {}", e), ToastSeverity::Error);
        (0, 0)
    });
    view_state.header_bar.set_repo_info(
        view_state.header_bar.repo_name.clone(),
        current,
        ahead,
        behind,
    );
}
