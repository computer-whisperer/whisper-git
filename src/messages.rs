use std::sync::mpsc::Receiver;

use git2::Oid;

use crate::git::{self, CommitInfo, DiffFile, GitRepo, RemoteOpResult};
use crate::ui::Rect;
use crate::ui::widgets::{ToastManager, ToastSeverity};
use crate::views::{BranchSidebar, CommitDetailView, CommitGraphView, DiffView, StagingWell};

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
    /// Load diff and show it inline in the staging panel (path, staged)
    ViewDiffInline(String, bool),
    /// Stage a hunk from inline diff, then refresh the inline diff
    InlineStageHunk(String, usize),
    /// Unstage a hunk from inline diff, then refresh the inline diff
    InlineUnstageHunk(String, usize),
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
                eprintln!("Failed to stage {}: {}", path, e);
                toast_manager.push(
                    format!("Stage failed: {}", e),
                    ToastSeverity::Error,
                );
            }
        }
        AppMessage::UnstageFile(path) => {
            if let Err(e) = staging_repo.unstage_file(&path) {
                eprintln!("Failed to unstage {}: {}", path, e);
                toast_manager.push(
                    format!("Unstage failed: {}", e),
                    ToastSeverity::Error,
                );
            }
        }
        AppMessage::StageAll => {
            if let Ok(status) = staging_repo.status() {
                for file in &status.unstaged {
                    let _ = staging_repo.stage_file(&file.path);
                }
            }
        }
        AppMessage::UnstageAll => {
            if let Ok(status) = staging_repo.status() {
                for file in &status.staged {
                    let _ = staging_repo.unstage_file(&file.path);
                }
            }
        }
        AppMessage::Commit(message) => {
            if message.trim().is_empty() {
                toast_manager.push(
                    "Commit message cannot be empty".to_string(),
                    ToastSeverity::Error,
                );
                return true;
            }
            if !staging_repo.has_user_config() {
                toast_manager.push(
                    "Git user not configured. Run: git config user.name \"Your Name\" && git config user.email \"you@example.com\"".to_string(),
                    ToastSeverity::Error,
                );
                return true;
            }
            match staging_repo.commit(&message) {
                Ok(oid) => {
                    println!("Created commit: {}", oid);
                    refresh_repo_state(repo, commits, view_state, toast_manager);
                    view_state.staging_well.clear_and_focus();
                    toast_manager.push(
                        format!("Commit {}", &oid.to_string()[..7]),
                        ToastSeverity::Success,
                    );
                }
                Err(e) => {
                    eprintln!("Failed to commit: {}", e);
                    toast_manager.push(
                        format!("Commit failed: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        AppMessage::Fetch => {
            if view_state.fetch_receiver.is_some() {
                eprintln!("Fetch already in progress");
                return false;
            }
            if let Some(workdir) = repo.working_dir_path() {
                let remote = repo.default_remote().unwrap_or_else(|_| "origin".to_string());
                println!("Fetching from {}...", remote);
                let rx = git::fetch_remote_async(workdir, remote);
                *view_state.fetch_receiver = Some((rx, std::time::Instant::now()));
                view_state.header_bar.fetching = true;
            } else {
                eprintln!("No working directory for fetch");
            }
        }
        AppMessage::Pull => {
            if view_state.pull_receiver.is_some() {
                eprintln!("Pull already in progress");
                return false;
            }
            if let Some(workdir) = repo.working_dir_path() {
                let remote = repo.default_remote().unwrap_or_else(|_| "origin".to_string());
                let branch = repo.current_branch().unwrap_or_else(|_| "HEAD".to_string());
                println!("Pulling {} from {}...", branch, remote);
                let rx = git::pull_remote_async(workdir, remote, branch);
                *view_state.pull_receiver = Some((rx, std::time::Instant::now()));
                view_state.header_bar.pulling = true;
            } else {
                eprintln!("No working directory for pull");
            }
        }
        AppMessage::PullRebase => {
            if view_state.pull_receiver.is_some() {
                eprintln!("Pull already in progress");
                return false;
            }
            if let Some(workdir) = repo.working_dir_path() {
                let remote = repo.default_remote().unwrap_or_else(|_| "origin".to_string());
                let branch = repo.current_branch().unwrap_or_else(|_| "HEAD".to_string());
                println!("Pulling (rebase) {} from {}...", branch, remote);
                let rx = git::pull_rebase_async(workdir, remote, branch);
                *view_state.pull_receiver = Some((rx, std::time::Instant::now()));
                view_state.header_bar.pulling = true;
            } else {
                eprintln!("No working directory for pull --rebase");
            }
        }
        AppMessage::Push => {
            if view_state.push_receiver.is_some() {
                eprintln!("Push already in progress");
                return false;
            }
            if let Some(workdir) = repo.working_dir_path() {
                let remote = repo.default_remote().unwrap_or_else(|_| "origin".to_string());
                let branch = repo.current_branch().unwrap_or_else(|_| "HEAD".to_string());
                println!("Pushing {} to {}...", branch, remote);
                let rx = git::push_remote_async(workdir, remote, branch);
                *view_state.push_receiver = Some((rx, std::time::Instant::now()));
                view_state.header_bar.pushing = true;
            } else {
                eprintln!("No working directory for push");
            }
        }
        AppMessage::PushForce => {
            if view_state.push_receiver.is_some() {
                eprintln!("Push already in progress");
                return false;
            }
            if let Some(workdir) = repo.working_dir_path() {
                let remote = repo.default_remote().unwrap_or_else(|_| "origin".to_string());
                let branch = repo.current_branch().unwrap_or_else(|_| "HEAD".to_string());
                println!("Force pushing {} to {} (--force-with-lease)...", branch, remote);
                let rx = git::push_force_async(workdir, remote, branch);
                *view_state.push_receiver = Some((rx, std::time::Instant::now()));
                view_state.header_bar.pushing = true;
                toast_manager.push("Force pushing...", ToastSeverity::Info);
            } else {
                eprintln!("No working directory for push");
            }
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
                }
                Err(e) => {
                    eprintln!("Failed to load diff for {}: {}", oid, e);
                }
            }
        }
        AppMessage::ViewCommitFileDiff(oid, path) => {
            match repo.diff_file_in_commit(oid, &path) {
                Ok(diff_files) => {
                    view_state.diff_view.set_diff(diff_files, path);
                }
                Err(e) => {
                    eprintln!("Failed to load diff for file '{}': {}", path, e);
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
                    eprintln!("Failed to load diff for {}: {}", path, e);
                }
            }
        }
        AppMessage::CheckoutBranch(name) => {
            match repo.checkout_branch(&name) {
                Ok(()) => {
                    println!("Checked out branch: {}", name);
                    refresh_repo_state(repo, commits, view_state, toast_manager);
                    toast_manager.push(
                        format!("Switched to {}", name),
                        ToastSeverity::Success,
                    );
                }
                Err(e) => {
                    eprintln!("Failed to checkout branch '{}': {}", name, e);
                    toast_manager.push(
                        format!("Checkout failed: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        AppMessage::CheckoutRemoteBranch(remote, branch) => {
            match repo.checkout_remote_branch(&remote, &branch) {
                Ok(()) => {
                    println!("Checked out remote branch: {}/{}", remote, branch);
                    refresh_repo_state(repo, commits, view_state, toast_manager);
                    toast_manager.push(
                        format!("Switched to {}/{}", remote, branch),
                        ToastSeverity::Success,
                    );
                }
                Err(e) => {
                    eprintln!("Failed to checkout remote branch '{}/{}': {}", remote, branch, e);
                    toast_manager.push(
                        format!("Checkout failed: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        AppMessage::DeleteBranch(name) => {
            match repo.delete_branch(&name) {
                Ok(()) => {
                    println!("Deleted branch: {}", name);
                    refresh_repo_state(repo, commits, view_state, toast_manager);
                    toast_manager.push(
                        format!("Deleted branch {}", name),
                        ToastSeverity::Success,
                    );
                }
                Err(e) => {
                    eprintln!("Failed to delete branch '{}': {}", name, e);
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
                    eprintln!("Failed to stage hunk: {}", e);
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
                    eprintln!("Failed to unstage hunk: {}", e);
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
            let current_count = commits.len();
            let new_count = current_count + 50;
            if let Ok(new_commits) = repo.commit_graph(new_count) {
                *commits = new_commits;
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
            match repo.create_branch_at(&name, oid) {
                Ok(()) => {
                    refresh_repo_state(repo, commits, view_state, toast_manager);
                    toast_manager.push(
                        format!("Created branch '{}'", name),
                        ToastSeverity::Success,
                    );
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Create branch failed: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        AppMessage::CreateTag(name, oid) => {
            match repo.create_tag(&name, oid) {
                Ok(()) => {
                    refresh_repo_state(repo, commits, view_state, toast_manager);
                    toast_manager.push(
                        format!("Created tag '{}'", name),
                        ToastSeverity::Success,
                    );
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Create tag failed: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        AppMessage::DeleteTag(name) => {
            match repo.delete_tag(&name) {
                Ok(()) => {
                    refresh_repo_state(repo, commits, view_state, toast_manager);
                    toast_manager.push(
                        format!("Deleted tag '{}'", name),
                        ToastSeverity::Success,
                    );
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Delete tag failed: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
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
            if message.trim().is_empty() {
                toast_manager.push(
                    "Commit message cannot be empty".to_string(),
                    ToastSeverity::Error,
                );
                return true;
            }
            if !staging_repo.has_user_config() {
                toast_manager.push(
                    "Git user not configured. Run: git config user.name \"Your Name\" && git config user.email \"you@example.com\"".to_string(),
                    ToastSeverity::Error,
                );
                return true;
            }
            match staging_repo.amend_commit(&message) {
                Ok(oid) => {
                    println!("Amended commit: {}", oid);
                    refresh_repo_state(repo, commits, view_state, toast_manager);
                    view_state.staging_well.exit_amend_mode();
                    toast_manager.push(
                        format!("Amended {}", &oid.to_string()[..7]),
                        ToastSeverity::Success,
                    );
                }
                Err(e) => {
                    eprintln!("Failed to amend: {}", e);
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
            match repo.reset_to_commit(oid, mode) {
                Ok(()) => {
                    refresh_repo_state(repo, commits, view_state, toast_manager);
                    toast_manager.push(
                        format!("Reset ({}) to {}", mode_name, &oid.to_string()[..7]),
                        ToastSeverity::Success,
                    );
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Reset failed: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }

        AppMessage::AbortOperation => {
            match repo.cleanup_state() {
                Ok(()) => {
                    refresh_repo_state(repo, commits, view_state, toast_manager);
                    toast_manager.push("Operation aborted", ToastSeverity::Success);
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Abort failed: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }

        AppMessage::AddRemote(name, url) => {
            match repo.add_remote(&name, &url) {
                Ok(()) => {
                    toast_manager.push(
                        format!("Added remote '{}'", name),
                        ToastSeverity::Success,
                    );
                    refresh_repo_state(repo, commits, view_state, toast_manager);
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Failed to add remote: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }

        AppMessage::DeleteRemote(name) => {
            match repo.delete_remote(&name) {
                Ok(()) => {
                    toast_manager.push(
                        format!("Deleted remote '{}'", name),
                        ToastSeverity::Success,
                    );
                    refresh_repo_state(repo, commits, view_state, toast_manager);
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Failed to delete remote: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }

        AppMessage::RenameRemote(old_name, new_name) => {
            match repo.rename_remote(&old_name, &new_name) {
                Ok(()) => {
                    toast_manager.push(
                        format!("Renamed remote '{}' to '{}'", old_name, new_name),
                        ToastSeverity::Success,
                    );
                    refresh_repo_state(repo, commits, view_state, toast_manager);
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Failed to rename remote: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }

        AppMessage::SetRemoteUrl(name, url) => {
            match repo.set_remote_url(&name, &url) {
                Ok(()) => {
                    toast_manager.push(
                        format!("Updated URL for '{}'", name),
                        ToastSeverity::Success,
                    );
                    refresh_repo_state(repo, commits, view_state, toast_manager);
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Failed to update remote URL: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }

        AppMessage::ViewDiffInline(path, staged) => {
            match staging_repo.diff_working_file(&path, staged) {
                Ok(hunks) => {
                    let diff_file = DiffFile::from_hunks(path.clone(), hunks);
                    view_state.staging_well.show_inline_diff(path, vec![diff_file], staged);
                }
                Err(e) => {
                    eprintln!("Failed to load inline diff for {}: {}", path, e);
                }
            }
        }
        AppMessage::InlineStageHunk(path, hunk_idx) => {
            match staging_repo.stage_hunk(&path, hunk_idx) {
                Ok(()) => {
                    toast_manager.push(
                        format!("Staged hunk {} in {}", hunk_idx + 1, path),
                        ToastSeverity::Success,
                    );
                    // Refresh the inline diff to show remaining hunks
                    if let Ok(hunks) = staging_repo.diff_working_file(&path, false) {
                        if hunks.is_empty() {
                            view_state.staging_well.close_inline_diff();
                        } else {
                            let diff_file = DiffFile::from_hunks(path.clone(), hunks);
                            view_state.staging_well.show_inline_diff(path, vec![diff_file], false);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Failed to stage hunk: {}", e);
                    toast_manager.push(
                        format!("Stage hunk failed: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        AppMessage::InlineUnstageHunk(path, hunk_idx) => {
            match staging_repo.unstage_hunk(&path, hunk_idx) {
                Ok(()) => {
                    toast_manager.push(
                        format!("Unstaged hunk {} in {}", hunk_idx + 1, path),
                        ToastSeverity::Success,
                    );
                    // Refresh the inline diff to show remaining hunks
                    if let Ok(hunks) = staging_repo.diff_working_file(&path, true) {
                        if hunks.is_empty() {
                            view_state.staging_well.close_inline_diff();
                        } else {
                            let diff_file = DiffFile::from_hunks(path.clone(), hunks);
                            view_state.staging_well.show_inline_diff(path, vec![diff_file], true);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Failed to unstage hunk: {}", e);
                    toast_manager.push(
                        format!("Unstage hunk failed: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
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
            eprintln!("Failed to load commit graph: {}", e);
            toast_manager.push(
                format!("Failed to load commits: {}", e),
                ToastSeverity::Error,
            );
            *commits = Vec::new();
        }
    }
    view_state.commit_graph_view.update_layout(commits);
    view_state.commit_graph_view.head_oid = repo.head_oid().ok();

    let branch_tips = repo.branch_tips().unwrap_or_else(|e| {
        eprintln!("Failed to load branch tips: {}", e);
        Vec::new()
    });
    let tags = repo.tags().unwrap_or_else(|e| {
        eprintln!("Failed to load tags: {}", e);
        Vec::new()
    });
    let current = repo.current_branch().unwrap_or_else(|e| {
        eprintln!("Failed to get current branch: {}", e);
        String::new()
    });

    let worktrees = repo.worktrees().unwrap_or_else(|e| {
        eprintln!("Failed to load worktrees: {}", e);
        Vec::new()
    });
    view_state.commit_graph_view.branch_tips = branch_tips.clone();
    view_state.commit_graph_view.tags = tags.clone();
    view_state.commit_graph_view.worktrees = worktrees.clone();
    view_state.branch_sidebar.set_branch_data(&branch_tips, &tags, current.clone());
    let current_workdir = repo.workdir().unwrap_or(std::path::Path::new(""));
    view_state.staging_well.set_worktrees(&worktrees, current_workdir);
    view_state.branch_sidebar.worktrees = worktrees;

    let submodules = repo.submodules().unwrap_or_else(|e| {
        eprintln!("Failed to load submodules: {}", e);
        Vec::new()
    });
    view_state.submodule_strip.submodules = submodules.clone();
    view_state.branch_sidebar.submodules = submodules;

    let (ahead, behind) = repo.ahead_behind().unwrap_or_else(|e| {
        eprintln!("Failed to compute ahead/behind: {}", e);
        (0, 0)
    });
    view_state.header_bar.set_repo_info(
        view_state.header_bar.repo_name.clone(),
        current,
        ahead,
        behind,
    );
}
