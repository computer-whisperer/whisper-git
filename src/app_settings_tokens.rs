use super::*;

impl App {
    /// Apply settings dialog values to config and views.
    pub(crate) fn apply_settings_changes(&mut self) {
        let row_scale = self.settings_dialog.row_scale;
        let abbreviate_wt = self.settings_dialog.abbreviate_worktree_names;
        let time_strength = self.settings_dialog.time_spacing_strength;
        let fast_scroll = self.settings_dialog.scroll_speed >= 1.5;
        let ratchet_scroll = self.settings_dialog.ratchet_scroll;
        let orphans_changed =
            self.config.show_orphaned_commits != self.settings_dialog.show_orphaned_commits;
        if let Some(ref state) = self.state {
            for (repo_tab, view_state) in &mut self.tabs {
                view_state.commit_graph_view.row_scale = row_scale;
                view_state.commit_graph_view.abbreviate_worktree_names = abbreviate_wt;
                view_state.commit_graph_view.time_spacing_strength = time_strength;
                view_state.commit_graph_view.fast_scroll = fast_scroll;
                view_state.commit_graph_view.ratchet_scroll = ratchet_scroll;
                view_state
                    .commit_graph_view
                    .sync_metrics(&state.text_renderer);
                view_state
                    .commit_graph_view
                    .compute_row_offsets(&repo_tab.commits);
            }
        }
        self.config.avatars_enabled = self.settings_dialog.show_avatars;
        self.config.fast_scroll = fast_scroll;
        self.config.row_scale = self.settings_dialog.row_scale;
        self.config.abbreviate_worktree_names = self.settings_dialog.abbreviate_worktree_names;
        self.config.time_spacing_strength = self.settings_dialog.time_spacing_strength;
        self.config.show_orphaned_commits = self.settings_dialog.show_orphaned_commits;
        self.config.ratchet_scroll = self.settings_dialog.ratchet_scroll;
        if let Err(e) = self.config.save() {
            self.toast_manager.push(e, ToastSeverity::Error);
        }
        if orphans_changed {
            for tab_idx in 0..self.tabs.len() {
                self.trigger_repo_state_refresh_for_tab(tab_idx);
            }
        }
    }

    /// Open the token management dialog with current keychain state.
    pub(crate) fn open_token_dialog(&mut self) {
        let github_has_token = token_store::get_github_token().is_some()
            || self
                .config
                .github_token
                .as_ref()
                .is_some_and(|t| !t.is_empty());

        let mut gitlab_hosts: Vec<(String, bool)> = Vec::new();
        for host in self.config.gitlab_tokens.keys() {
            let has_token = token_store::get_gitlab_token(host).is_some()
                || self
                    .config
                    .gitlab_tokens
                    .get(host)
                    .is_some_and(|t| !t.is_empty());
            gitlab_hosts.push((host.clone(), has_token));
        }
        self.token_dialog.show(github_has_token, gitlab_hosts);
        self.active_modal = Some(ActiveModal::TokenManager);
    }

    /// Handle an action from the token dialog.
    pub(crate) fn handle_token_action(&mut self, action: TokenDialogAction) {
        match action {
            TokenDialogAction::Close => {}
            TokenDialogAction::SetGitHubToken(token) => {
                if token.is_empty() {
                    token_store::delete_github_token();
                    self.config.github_token = None;
                    let _ = self.config.save();
                    self.toast_manager
                        .push("GitHub token removed", ToastSeverity::Success);
                } else if token_store::set_github_token(&token) {
                    self.config.github_token = None;
                    let _ = self.config.save();
                    self.toast_manager
                        .push("GitHub token saved to keychain", ToastSeverity::Success);
                } else {
                    self.config.github_token = Some(token);
                    let _ = self.config.save();
                    self.toast_manager.push(
                        "GitHub token saved to config (keychain unavailable)",
                        ToastSeverity::Success,
                    );
                }
            }
            TokenDialogAction::SetGitLabToken { host, token } => {
                if token_store::set_gitlab_token(&host, &token) {
                    self.config
                        .gitlab_tokens
                        .insert(host.clone(), String::new());
                    let _ = self.config.save();
                    self.toast_manager.push(
                        format!("GitLab token for {host} saved to keychain"),
                        ToastSeverity::Success,
                    );
                } else {
                    self.config.gitlab_tokens.insert(host.clone(), token);
                    let _ = self.config.save();
                    self.toast_manager.push(
                        format!("GitLab token for {host} saved to config (keychain unavailable)"),
                        ToastSeverity::Success,
                    );
                }
            }
            TokenDialogAction::RemoveGitLabToken(host) => {
                token_store::delete_gitlab_token(&host);
                self.config.gitlab_tokens.remove(&host);
                let _ = self.config.save();
                self.toast_manager.push(
                    format!("GitLab token for {host} removed"),
                    ToastSeverity::Success,
                );
            }
        }
    }
}
