use super::*;

impl App {
    /// Diagnostic reload: capture current UI state, kick off async re-read.
    /// The "after" snapshot and delta report are produced when the background
    /// results arrive (see `finalize_diagnostic_reload`).
    pub(crate) fn do_diagnostic_reload(&mut self) {
        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };

        // 1. Capture "before" snapshot from current UI state
        let before = {
            let msg_view = MessageViewState {
                commit_graph_view: &mut view_state.commit_graph_view,
                staging_well: &mut view_state.staging_well,
                diff_view: &mut view_state.diff_view,
                commit_detail_view: &mut view_state.commit_detail_view,
                branch_sidebar: &mut view_state.branch_sidebar,
                header_bar: &mut view_state.header_bar,
                last_diff_commit: &mut view_state.last_diff_commit,
                fetch_receiver: &mut view_state.fetch_receiver,
                pull_receiver: &mut view_state.pull_receiver,
                push_receiver: &mut view_state.push_receiver,
                generic_op_receiver: &mut view_state.generic_op_receiver,
                right_panel_mode: &mut view_state.right_panel_mode,
                worktrees: &mut view_state.worktree_state.worktrees,
                proxy: self.proxy.clone(),
                needs_repo_refresh: false,
            };
            RepoStateSnapshot::from_ui(
                &repo_tab.commits,
                &msg_view,
                &view_state.current_branch,
                view_state.head_oid,
            )
        };

        // 2. Reopen repos to bypass libgit2 cache
        let _ = repo_tab.repo.reopen();
        for wt_repo in view_state.worktree_state.repo_cache.values_mut() {
            let _ = wt_repo.reopen();
        }

        // 3. Store "before" snapshot and kick off async refresh
        self.diagnostic_before = Some(before);
        self.trigger_repo_state_refresh();
        self.status_dirty = true;
    }

    /// Finalize a diagnostic reload once both repo state and status results have
    /// been applied. Captures the "after" snapshot, computes deltas, writes report.
    pub(crate) fn finalize_diagnostic_reload(&mut self) {
        // Only finalize when both async results have been consumed
        let active_pending = self
            .tabs
            .get(self.active_tab)
            .is_some_and(|(_, view_state)| {
                view_state.repo_state_receiver.is_some() || view_state.status_receiver.is_some()
            });
        if active_pending {
            return;
        }
        let Some(before) = self.diagnostic_before.take() else {
            return;
        };
        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };

        // Capture "after" snapshot
        let after = {
            let msg_view = MessageViewState {
                commit_graph_view: &mut view_state.commit_graph_view,
                staging_well: &mut view_state.staging_well,
                diff_view: &mut view_state.diff_view,
                commit_detail_view: &mut view_state.commit_detail_view,
                branch_sidebar: &mut view_state.branch_sidebar,
                header_bar: &mut view_state.header_bar,
                last_diff_commit: &mut view_state.last_diff_commit,
                fetch_receiver: &mut view_state.fetch_receiver,
                pull_receiver: &mut view_state.pull_receiver,
                push_receiver: &mut view_state.push_receiver,
                generic_op_receiver: &mut view_state.generic_op_receiver,
                right_panel_mode: &mut view_state.right_panel_mode,
                worktrees: &mut view_state.worktree_state.worktrees,
                proxy: self.proxy.clone(),
                needs_repo_refresh: false,
            };
            RepoStateSnapshot::from_ui(
                &repo_tab.commits,
                &msg_view,
                &view_state.current_branch,
                view_state.head_oid,
            )
        };

        // Compare and write report to file
        let deltas = compute_reload_deltas(&before, &after);
        let report_dir = std::env::var("HOME")
            .map(|h| {
                std::path::PathBuf::from(h)
                    .join(".config")
                    .join("whisper-git")
                    .join("reload-reports")
            })
            .ok();
        if let Some(ref dir) = report_dir {
            let _ = std::fs::create_dir_all(dir);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let path = dir.join(format!("reload-{}.txt", now));
            let mut report = String::new();
            report.push_str(&format!("Reload report — unix {}\n", now));
            if let Some((repo_tab, _)) = self.tabs.get(self.active_tab) {
                report.push_str(&format!("Repo: {}\n", repo_tab.name));
            }
            report.push_str(&format!("Deltas: {}\n\n", deltas.len()));
            if deltas.is_empty() {
                report.push_str("No deltas detected.\n");
            } else {
                for delta in &deltas {
                    report.push_str(delta);
                    report.push('\n');
                }
            }
            report.push_str("\n--- Before snapshot ---\n");
            report.push_str(&format!(
                "Commits: {} (non-synthetic)\n",
                before.commit_oids.len()
            ));
            report.push_str(&format!("HEAD: {:?}\n", before.head_oid));
            report.push_str(&format!("Branch: {}\n", before.current_branch));
            report.push_str(&format!("Branch tips: {}\n", before.branch_tips.len()));
            report.push_str(&format!("Tags: {}\n", before.tags.len()));
            report.push_str(&format!(
                "Staged/Unstaged/Conflicted: {}/{}/{}\n",
                before.staged_count, before.unstaged_count, before.conflicted_count
            ));
            report.push_str("\n--- After snapshot ---\n");
            report.push_str(&format!(
                "Commits: {} (non-synthetic)\n",
                after.commit_oids.len()
            ));
            report.push_str(&format!("HEAD: {:?}\n", after.head_oid));
            report.push_str(&format!("Branch: {}\n", after.current_branch));
            report.push_str(&format!("Branch tips: {}\n", after.branch_tips.len()));
            report.push_str(&format!("Tags: {}\n", after.tags.len()));
            report.push_str(&format!(
                "Staged/Unstaged/Conflicted: {}/{}/{}\n",
                after.staged_count, after.unstaged_count, after.conflicted_count
            ));

            match std::fs::write(&path, &report) {
                Ok(()) => {
                    let summary = if deltas.is_empty() {
                        "Reload: no deltas".to_string()
                    } else {
                        format!(
                            "Reload: {} delta{}",
                            deltas.len(),
                            if deltas.len() == 1 { "" } else { "s" }
                        )
                    };
                    self.toast_manager.push(
                        format!("{} — {}", summary, path.display()),
                        if deltas.is_empty() {
                            ToastSeverity::Success
                        } else {
                            ToastSeverity::Info
                        },
                    );
                }
                Err(e) => {
                    self.toast_manager.push(
                        format!("Failed to write reload report: {}", e),
                        ToastSeverity::Error,
                    );
                }
            }
        }

        self.status_dirty = true;
    }
}
