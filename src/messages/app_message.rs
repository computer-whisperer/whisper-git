use std::path::PathBuf;

use git2::Oid;

use crate::ui::Rect;

/// What content mode the right panel is showing
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RightPanelMode {
    /// Default: file lists + commit message + buttons (upper), selected file diff (lower)
    #[default]
    Staging,
    /// Shown when a commit is selected in graph: commit detail (upper), file diff (lower)
    Browse,
}

/// Application-level messages for state changes
#[derive(Clone, Debug)]
pub enum AppMessage {
    StageFile(String),
    UnstageFile(String),
    StageAll,
    UnstageAll,
    Commit(String),
    Fetch(Option<String>),
    Pull {
        remote: Option<String>,
        branch: String,
    },
    PullRebase {
        remote: Option<String>,
        branch: String,
    },
    ShowPullDialog(String), // (branch) — caller specifies which branch
    PullBranchFrom {
        remote: String,
        branch: String,
        rebase: bool,
    },
    Push {
        remote: Option<String>,
        branch: String,
    },
    PushForce {
        remote: Option<String>,
        branch: String,
    },
    ShowPushDialog(String), // (branch) — caller specifies which branch
    PushBranchTo {
        local_branch: String,
        remote: String,
        remote_branch: String,
        force: bool,
    },
    SelectedCommit(Oid),
    ViewCommitFileDiff(Oid, String),
    ViewDiff(String, bool), // (path, staged)
    CheckoutBranch(String),
    CheckoutRemoteBranch(String, String),
    CheckoutCommit(Oid, Option<PathBuf>), // (commit_oid, target_worktree_dir)
    DeleteBranch(String),
    RenameBranch(String, String), // (old_name, new_name)
    StageHunk(String, usize),     // (file_path, hunk_index)
    UnstageHunk(String, usize),   // (file_path, hunk_index)
    DiscardFile(String),
    DiscardFiles(Vec<String>),
    DiscardHunk(String, usize), // (file_path, hunk_index)
    LoadMoreCommits,
    DeleteSubmodule(String),
    UpdateSubmodule(String),
    ResetSubmodule(String),
    JumpToWorktreeBranch(String),
    JumpToCommit(Oid),
    RemoveWorktree(String),
    MergeBranch(String, Option<PathBuf>), // (branch, target_worktree_dir)
    MergeNoFf(String, String, Option<PathBuf>), // (branch, commit_message, target_worktree_dir)
    MergeFfOnly(String, Option<PathBuf>),
    MergeSquash(String, Option<PathBuf>),
    RebaseBranchWithOptions(String, bool, bool, Option<PathBuf>), // (branch, autostash, rebase_merges, target_worktree_dir)
    CreateBranch(String, Oid),                                    // (name, at_commit)
    CreateTag(String, Oid),                                       // (name, at_commit)
    DeleteTag(String),
    StashPush,
    StashPop,
    StashApply(usize),
    StashDrop(usize),
    StashPopIndex(usize),
    CherryPick(Oid, Option<PathBuf>),
    AmendCommit(String),
    ToggleAmend,
    RevertCommit(Oid, Option<PathBuf>),
    ResetToCommit(Oid, git2::ResetType, Option<PathBuf>),
    EnterSubmodule(String),
    ExitSubmodule,
    ExitToDepth(usize),
    AbortOperation,
    CreateWorktree(String, String, bool, bool), // (name, source_ref, init_submodules, checkout_lfs)
    AddRemote(String, String),                  // (name, url)
    DeleteRemote(String),
    RenameRemote(String, String),       // (old_name, new_name)
    SetRemoteUrl(String, String),       // (name, new_url)
    DeleteRemoteBranch(String, String), // (remote, branch)
    FetchAll,
    CheckoutBranchInWorktree(String, PathBuf), // (branch, worktree_path)
    SetHead(String),                           // bare-repo HEAD pointer update
    StageAllUntracked,
    AiGenerateCommitMessage,
}

/// Context needed by `handle_app_message` that lives outside the per-tab
/// state. The caller constructs this from `App` fields before entering the
/// message loop.
pub struct MessageContext {
    /// Graph bounds for scroll-to-selection (JumpToWorktreeBranch).
    /// Compute this from the current layout before calling the handler.
    pub graph_bounds: Rect,
    /// Whether to include orphaned commits when refreshing the commit graph.
    pub show_orphaned_commits: bool,
}
