//! Submodule drill-down/drill-up navigation, tab view initialization,
//! filesystem watcher setup, and terminal launching.

use std::sync::mpsc::Receiver;
use winit::event_loop::EventLoopProxy;

use crate::async_polling::{RepoStateResult, spawn_repo_state_refresh};
use crate::git::GitRepo;
use crate::ui::TextRenderer;
use crate::ui::widgets::{ToastManager, ToastSeverity};
use crate::watcher::RepoWatcher;

use super::{MAX_COMMITS, RepoTab, SavedParentState, SubmoduleFocus, TabViewState, WorktreeState};

/// Initialize a tab's view state from its repo data
pub(crate) fn init_tab_view(
    repo_tab: &mut RepoTab,
    view_state: &mut TabViewState,
    text_renderer: &TextRenderer,
    scale: f32,
    toast_manager: &mut ToastManager,
    show_orphaned_commits: bool,
    proxy: &EventLoopProxy<()>,
) -> Receiver<RepoStateResult> {
    // Sync view metrics to the current text renderer scale
    view_state.commit_graph_view.sync_metrics(text_renderer);
    view_state.branch_sidebar.sync_metrics(text_renderer);
    view_state.staging_well.set_scale(scale);

    // Set initial repo path in header — use common_dir parent to show project path,
    // not a worktree-specific path.
    let project_path = repo_tab
        .repo
        .common_dir()
        .parent()
        .unwrap_or(repo_tab.repo.common_dir());
    let repo_path_str = project_path.to_string_lossy().into_owned();
    let repo_path_str = repo_path_str.trim_end_matches('/').to_string();
    view_state.header_bar.set_repo_path(&repo_path_str);

    // No worktree is auto-selected at init. The user picks one via the staging well selector.
    // worktree_state.selected_path stays None until explicitly set.

    // Spawn async repo state refresh
    let repo_git_dir = repo_tab.repo.git_dir().to_path_buf();
    let rx = spawn_repo_state_refresh(repo_git_dir, None, show_orphaned_commits, proxy.clone());

    // Start filesystem watcher for auto-refresh
    start_watcher(repo_tab, view_state, toast_manager, proxy);

    rx
}

/// Start (or restart) a filesystem watcher for the given tab's repo.
pub(crate) fn start_watcher(
    repo_tab: &RepoTab,
    view_state: &mut TabViewState,
    toast_manager: &mut ToastManager,
    proxy: &EventLoopProxy<()>,
) {
    // Drop any existing watcher first
    view_state.watcher = None;
    view_state.watcher_rx = None;

    let repo = &repo_tab.repo;
    let Some(workdir) = repo.workdir() else {
        return;
    };
    let git_dir = repo.git_dir();
    let common_dir = repo.common_dir();

    match RepoWatcher::new(
        workdir,
        git_dir,
        common_dir,
        &view_state.worktree_state.worktrees,
        proxy.clone(),
    ) {
        Ok((watcher, rx)) => {
            view_state.watcher = Some(watcher);
            view_state.watcher_rx = Some(rx);
        }
        Err(e) => {
            toast_manager.push(
                format!("Filesystem watcher failed: {}", e),
                ToastSeverity::Error,
            );
        }
    }
}

/// Drill into a named submodule: saves parent state and swaps repo to the submodule.
/// Returns true on success.
#[allow(clippy::too_many_arguments)]
pub(crate) fn enter_submodule(
    name: &str,
    repo_tab: &mut RepoTab,
    view_state: &mut TabViewState,
    text_renderer: &TextRenderer,
    scale: f32,
    toast_manager: &mut ToastManager,
    show_orphaned_commits: bool,
    proxy: &EventLoopProxy<()>,
) -> Option<Receiver<RepoStateResult>> {
    // Find the submodule info by name
    let sm = view_state
        .staging_well
        .submodules
        .iter()
        .find(|s| s.name == name)
        .cloned();
    let Some(sm) = sm else {
        toast_manager.push(
            format!("Submodule '{}' not found", name),
            ToastSeverity::Error,
        );
        return None;
    };

    // Resolve submodule path relative to the active worktree's workdir
    let parent_workdir = match view_state.staging_well.active_worktree_path() {
        Some(path) => path,
        None => {
            toast_manager.push("No active worktree".to_string(), ToastSeverity::Error);
            return None;
        }
    };
    let sub_path = parent_workdir.join(sm.path);

    // Open the submodule as a repo
    let sub_repo = match GitRepo::open(sub_path) {
        Ok(r) => r,
        Err(e) => {
            toast_manager.push(
                format!("Cannot open submodule '{}': {}", name, e),
                ToastSeverity::Error,
            );
            return None;
        }
    };

    // Save parent state — use std::mem::replace for atomic swap (repo is non-optional)
    let parent_repo = std::mem::replace(&mut repo_tab.repo, sub_repo);
    let parent_commits = std::mem::take(&mut repo_tab.commits);
    let parent_name = repo_tab.name.clone();
    let parent_submodules = view_state.staging_well.submodules.clone();

    let saved = SavedParentState {
        repo: parent_repo,
        commits: parent_commits,
        repo_name: parent_name,
        graph_scroll_offset: view_state.commit_graph_view.scroll_offset,
        graph_top_row_index: view_state.commit_graph_view.top_row_index,
        selected_commit: view_state.commit_graph_view.selected_commit,
        sidebar_scroll_offset: view_state.branch_sidebar.scroll_offset,
        submodule_name: name.to_string(),
        parent_submodules,
        worktree_state: view_state.worktree_state.save(),
    };

    // Clear diff/detail views and worktree state for the submodule
    view_state.diff_view.clear();
    view_state.commit_detail_view.clear();
    view_state.last_diff_commit = None;
    view_state.worktree_state = WorktreeState::new();

    // Clear staging well immediately to avoid showing stale parent files
    view_state.staging_well.clear_status();

    // Swap in submodule data (repo already swapped via std::mem::replace above)
    let sub_commits = repo_tab.repo.commit_graph(MAX_COMMITS).unwrap_or_default();
    repo_tab.name = name.to_string();
    repo_tab.commits = sub_commits;

    // Build/extend focus state
    match &mut view_state.submodule_focus {
        Some(focus) => {
            focus.parent_stack.push(saved);
            focus.current_name = name.to_string();
        }
        None => {
            view_state.submodule_focus = Some(SubmoduleFocus {
                parent_stack: vec![saved],
                current_name: name.to_string(),
            });
        }
    }

    // Re-init views with the submodule data
    let rx = init_tab_view(
        repo_tab,
        view_state,
        text_renderer,
        scale,
        toast_manager,
        show_orphaned_commits,
        proxy,
    );

    Some(rx)
}

/// Pop one level from the submodule focus stack, restoring parent state.
/// Returns a receiver for the async repo state refresh, or None on failure.
pub(crate) fn exit_submodule(
    repo_tab: &mut RepoTab,
    view_state: &mut TabViewState,
    text_renderer: &TextRenderer,
    scale: f32,
    toast_manager: &mut ToastManager,
    show_orphaned_commits: bool,
    proxy: &EventLoopProxy<()>,
) -> Option<Receiver<RepoStateResult>> {
    // Pop saved state from the focus stack (release borrow before init_tab_view)
    let saved = {
        let focus = view_state.submodule_focus.as_mut()?;
        focus.parent_stack.pop()?
    };

    // Clear diff/detail
    view_state.diff_view.clear();
    view_state.commit_detail_view.clear();
    view_state.last_diff_commit = None;

    // Restore parent data
    let scroll_offset = saved.graph_scroll_offset;
    let top_row_index = saved.graph_top_row_index;
    let selected = saved.selected_commit;
    let sidebar_scroll = saved.sidebar_scroll_offset;
    let parent_submodules = saved.parent_submodules;

    repo_tab.repo = saved.repo;
    repo_tab.commits = saved.commits;
    repo_tab.name = saved.repo_name;
    view_state
        .worktree_state
        .restore(saved.worktree_state, &repo_tab.repo);

    // Re-init views with parent data
    let rx = init_tab_view(
        repo_tab,
        view_state,
        text_renderer,
        scale,
        toast_manager,
        show_orphaned_commits,
        proxy,
    );

    // Restore scroll/selection
    view_state.commit_graph_view.scroll_offset = scroll_offset;
    view_state.commit_graph_view.top_row_index = top_row_index;
    view_state.commit_graph_view.selected_commit = selected;
    view_state.branch_sidebar.scroll_offset = sidebar_scroll;

    // Restore submodule siblings in staging well
    view_state.staging_well.set_submodules(parent_submodules);

    // If stack is now empty, clear focus entirely
    let stack_empty = view_state
        .submodule_focus
        .as_ref()
        .map(|f| f.parent_stack.is_empty())
        .unwrap_or(true);
    if stack_empty {
        view_state.submodule_focus = None;
    } else if let Some(ref mut focus) = view_state.submodule_focus {
        // Update current_name to the parent that's now active
        focus.current_name = focus
            .parent_stack
            .last()
            .map(|s| s.submodule_name.clone())
            .unwrap_or_default();
    }

    Some(rx)
}

/// Pop multiple levels to reach the given depth (0 = root).
#[allow(clippy::too_many_arguments)]
pub(crate) fn exit_to_depth(
    depth: usize,
    repo_tab: &mut RepoTab,
    view_state: &mut TabViewState,
    text_renderer: &TextRenderer,
    scale: f32,
    toast_manager: &mut ToastManager,
    show_orphaned_commits: bool,
    proxy: &EventLoopProxy<()>,
) -> Option<Receiver<RepoStateResult>> {
    let current_depth = view_state
        .submodule_focus
        .as_ref()
        .map(|f| f.parent_stack.len())
        .unwrap_or(0);
    if depth >= current_depth {
        return None;
    }
    let pops = current_depth - depth;
    let mut last_rx = None;
    for _ in 0..pops {
        match exit_submodule(
            repo_tab,
            view_state,
            text_renderer,
            scale,
            toast_manager,
            show_orphaned_commits,
            proxy,
        ) {
            Some(rx) => last_rx = Some(rx),
            None => break,
        }
    }
    last_rx
}

/// Try to open a terminal emulator at the given directory path.
/// Checks $TERMINAL env var first, then falls back to common terminals.
pub(crate) fn open_terminal_at(dir: &str, label: &str, toast_manager: &mut ToastManager) {
    use std::process::Command;

    let path = std::path::Path::new(dir);
    if !path.exists() {
        toast_manager.push(
            format!("Path does not exist: {}", dir),
            ToastSeverity::Error,
        );
        return;
    }

    // Check $TERMINAL env var first, then try common terminal emulators
    let candidates: Vec<String> = if let Ok(term) = std::env::var("TERMINAL") {
        std::iter::once(term)
            .chain(
                [
                    "kitty",
                    "alacritty",
                    "wezterm",
                    "foot",
                    "xterm",
                    "gnome-terminal",
                    "konsole",
                ]
                .iter()
                .map(|s| s.to_string()),
            )
            .collect()
    } else {
        [
            "kitty",
            "alacritty",
            "wezterm",
            "foot",
            "xterm",
            "gnome-terminal",
            "konsole",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    };

    for terminal in &candidates {
        let result = if terminal == "gnome-terminal" {
            Command::new(terminal)
                .arg("--working-directory")
                .arg(dir)
                .spawn()
        } else {
            // Most terminals accept --working-directory or use the cwd
            Command::new(terminal).current_dir(dir).spawn()
        };

        match result {
            Ok(_) => {
                toast_manager.push(
                    format!("Opened {} in {}", label, terminal),
                    ToastSeverity::Success,
                );
                return;
            }
            Err(_) => continue,
        }
    }

    toast_manager.push(
        "No terminal emulator found. Set $TERMINAL env var.".to_string(),
        ToastSeverity::Info,
    );
}
