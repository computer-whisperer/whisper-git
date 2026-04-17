use crate::git::GitRepo;
use crate::ui::widgets::{ToastManager, ToastSeverity};

use super::{AppMessage, MessageViewState};

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

/// Handle staging/index and commit/amend style messages.
/// Returns `Some(handled)` when the message belongs to this domain.
pub(super) fn handle_staging_commit_message(
    msg: &AppMessage,
    staging_repo: &GitRepo,
    view_state: &mut MessageViewState<'_>,
    toast_manager: &mut ToastManager,
) -> Option<bool> {
    match msg {
        AppMessage::StageFile(path) => {
            if let Err(e) = staging_repo.stage_file(path) {
                toast_manager.push(format!("Stage failed: {}", e), ToastSeverity::Error);
            }
            Some(true)
        }
        AppMessage::UnstageFile(path) => {
            if let Err(e) = staging_repo.unstage_file(path) {
                toast_manager.push(format!("Unstage failed: {}", e), ToastSeverity::Error);
            }
            Some(true)
        }
        AppMessage::StageAll => {
            match staging_repo.status() {
                Ok(status) => {
                    let total = status.unstaged.len();
                    if total == 0 {
                        toast_manager.push("No unstaged files".to_string(), ToastSeverity::Info);
                    } else {
                        let mut failed = 0;
                        for file in &status.unstaged {
                            if staging_repo.stage_file(&file.path).is_err() {
                                failed += 1;
                            }
                        }
                        if failed > 0 {
                            toast_manager.push(
                                format!(
                                    "Staged {}/{} files ({} failed)",
                                    total - failed,
                                    total,
                                    failed
                                ),
                                ToastSeverity::Error,
                            );
                        } else {
                            toast_manager.push(
                                format!(
                                    "Staged {} file{}",
                                    total,
                                    if total == 1 { "" } else { "s" }
                                ),
                                ToastSeverity::Success,
                            );
                        }
                    }
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Failed to read file status: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
            Some(true)
        }
        AppMessage::StageAllUntracked => {
            match staging_repo.status() {
                Ok(status) => {
                    let total = status.untracked.len();
                    if total == 0 {
                        toast_manager.push("No untracked files".to_string(), ToastSeverity::Info);
                    } else {
                        let mut failed = 0;
                        for file in &status.untracked {
                            if staging_repo.stage_file(&file.path).is_err() {
                                failed += 1;
                            }
                        }
                        if failed > 0 {
                            toast_manager.push(
                                format!(
                                    "Staged {}/{} files ({} failed)",
                                    total - failed,
                                    total,
                                    failed
                                ),
                                ToastSeverity::Error,
                            );
                        } else {
                            toast_manager.push(
                                format!(
                                    "Tracked {} file{}",
                                    total,
                                    if total == 1 { "" } else { "s" }
                                ),
                                ToastSeverity::Success,
                            );
                        }
                    }
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Failed to read file status: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
            Some(true)
        }
        AppMessage::UnstageAll => {
            match staging_repo.status() {
                Ok(status) => {
                    let total = status.staged.len();
                    if total == 0 {
                        toast_manager.push("No staged files".to_string(), ToastSeverity::Info);
                    } else {
                        let mut failed = 0;
                        for file in &status.staged {
                            if staging_repo.unstage_file(&file.path).is_err() {
                                failed += 1;
                            }
                        }
                        if failed > 0 {
                            toast_manager.push(
                                format!(
                                    "Unstaged {}/{} files ({} failed)",
                                    total - failed,
                                    total,
                                    failed
                                ),
                                ToastSeverity::Error,
                            );
                        } else {
                            toast_manager.push(
                                format!(
                                    "Unstaged {} file{}",
                                    total,
                                    if total == 1 { "" } else { "s" }
                                ),
                                ToastSeverity::Success,
                            );
                        }
                    }
                }
                Err(e) => {
                    toast_manager.push(
                        format!("Failed to read file status: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
            Some(true)
        }
        AppMessage::Commit(message) => {
            if !validate_commit_preconditions(message, staging_repo, toast_manager) {
                return Some(true);
            }
            match staging_repo.commit(message) {
                Ok(oid) => {
                    view_state.needs_repo_refresh = true;
                    view_state.staging_well.clear_and_focus();
                    toast_manager.push(
                        format!("Commit {}", &oid.to_string()[..7]),
                        ToastSeverity::Success,
                    );
                }
                Err(e) => {
                    toast_manager.push(format!("Commit failed: {}", e), ToastSeverity::Error);
                }
            }
            Some(true)
        }
        AppMessage::AmendCommit(message) => {
            if !validate_commit_preconditions(message, staging_repo, toast_manager) {
                return Some(true);
            }
            match staging_repo.amend_commit(message) {
                Ok(oid) => {
                    view_state.needs_repo_refresh = true;
                    view_state.staging_well.exit_amend_mode();
                    toast_manager.push(
                        format!("Amended {}", &oid.to_string()[..7]),
                        ToastSeverity::Success,
                    );
                }
                Err(e) => {
                    toast_manager.push(format!("Amend failed: {}", e), ToastSeverity::Error);
                }
            }
            Some(true)
        }
        AppMessage::ToggleAmend => {
            if view_state.staging_well.amend_mode {
                view_state.staging_well.exit_amend_mode();
            } else if let Some((subject, body)) = staging_repo.head_commit_message() {
                view_state.staging_well.enter_amend_mode(&subject, &body);
            } else {
                toast_manager.push("No HEAD commit to amend".to_string(), ToastSeverity::Error);
            }
            Some(true)
        }
        _ => None,
    }
}
