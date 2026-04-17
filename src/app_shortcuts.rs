use super::*;

impl App {
    /// Handle global keyboard shortcuts (Ctrl+O, Ctrl+W, Ctrl+Tab, etc.).
    /// Returns true if the event was consumed.
    pub(crate) fn handle_global_shortcuts(&mut self, input_event: &InputEvent) -> bool {
        let InputEvent::KeyDown { key, modifiers, .. } = input_event else {
            return false;
        };

        // Ctrl+O: open repo
        if *key == Key::O && modifiers.only_ctrl() {
            self.repo_dialog.show_with_recent(&self.config.recent_repos);
            self.active_modal = Some(ActiveModal::RepoDialog);
            return true;
        }
        // Ctrl+Shift+O: clone repo
        if *key == Key::O && modifiers.ctrl && modifiers.shift && !modifiers.alt {
            let gh_token =
                token_store::get_github_token().or_else(|| self.config.github_token.clone());
            self.clone_dialog.show(gh_token.as_deref());
            self.active_modal = Some(ActiveModal::CloneDialog);
            return true;
        }
        // Ctrl+W: close tab
        if *key == Key::W && modifiers.only_ctrl() {
            if self.tabs.len() > 1 {
                let idx = self.active_tab;
                self.close_tab(idx);
            }
            return true;
        }
        // Ctrl+Tab: next tab
        if *key == Key::Tab && modifiers.only_ctrl() {
            let next = (self.active_tab + 1) % self.tabs.len();
            self.switch_tab(next);
            return true;
        }
        // Ctrl+Shift+Tab: previous tab
        if *key == Key::Tab && modifiers.ctrl_shift() {
            let prev = if self.active_tab == 0 {
                self.tabs.len() - 1
            } else {
                self.active_tab - 1
            };
            self.switch_tab(prev);
            return true;
        }
        // Ctrl+S: stash push (only when staging text inputs are not focused)
        if *key == Key::S
            && modifiers.only_ctrl()
            && let Some((_, view_state)) = self.tabs.get_mut(self.active_tab)
            && !view_state.staging_well.has_text_focus()
        {
            view_state.pending_messages.push(AppMessage::StashPush);
            return true;
        }
        // Ctrl+Shift+S: stash pop
        if *key == Key::S
            && modifiers.ctrl_shift()
            && let Some((_, view_state)) = self.tabs.get_mut(self.active_tab)
        {
            view_state.pending_messages.push(AppMessage::StashPop);
            return true;
        }
        // Ctrl+Shift+A: toggle amend mode
        if *key == Key::A
            && modifiers.ctrl_shift()
            && let Some((_, view_state)) = self.tabs.get_mut(self.active_tab)
            && !view_state.staging_well.has_text_focus()
        {
            view_state.pending_messages.push(AppMessage::ToggleAmend);
            return true;
        }
        // F5: diagnostic reload
        if *key == Key::F5 && !modifiers.any() {
            self.do_diagnostic_reload();
            return true;
        }
        // Ctrl+1..9: switch worktree context in staging well
        if modifiers.only_ctrl() {
            let wt_index = match *key {
                Key::Num1 => Some(0),
                Key::Num2 => Some(1),
                Key::Num3 => Some(2),
                Key::Num4 => Some(3),
                Key::Num5 => Some(4),
                Key::Num6 => Some(5),
                Key::Num7 => Some(6),
                Key::Num8 => Some(7),
                Key::Num9 => Some(8),
                _ => None,
            };
            if let Some(idx) = wt_index
                && let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab)
                && view_state.staging_well.has_worktree_selector()
                && idx < view_state.staging_well.worktree_count()
            {
                view_state.switch_to_worktree(idx, &repo_tab.repo);
                return true;
            }
        }
        // Ctrl+Shift+F: Fetch
        if *key == Key::F
            && modifiers.ctrl_shift()
            && let Some((_, view_state)) = self.tabs.get_mut(self.active_tab)
        {
            view_state.pending_messages.push(AppMessage::Fetch(None));
            return true;
        }
        // Ctrl+Shift+L: Pull
        if *key == Key::L
            && modifiers.ctrl_shift()
            && let Some((_, view_state)) = self.tabs.get_mut(self.active_tab)
        {
            let branch = view_state
                .current_branch_opt()
                .unwrap_or("HEAD")
                .to_string();
            view_state.pending_messages.push(AppMessage::Pull {
                remote: None,
                branch,
            });
            return true;
        }
        // Ctrl+Shift+P: Push
        if *key == Key::P
            && modifiers.ctrl_shift()
            && let Some((_, view_state)) = self.tabs.get_mut(self.active_tab)
        {
            let branch = view_state
                .current_branch_opt()
                .unwrap_or("HEAD")
                .to_string();
            view_state.pending_messages.push(AppMessage::Push {
                remote: None,
                branch,
            });
            return true;
        }
        // Ctrl+Shift+R: Pull --rebase
        if *key == Key::R
            && modifiers.ctrl_shift()
            && let Some((_, view_state)) = self.tabs.get_mut(self.active_tab)
        {
            let branch = view_state
                .current_branch_opt()
                .unwrap_or("HEAD")
                .to_string();
            view_state.pending_messages.push(AppMessage::PullRebase {
                remote: None,
                branch,
            });
            return true;
        }
        // Backtick (`): Open terminal at repo workdir
        if *key == Key::Grave
            && !modifiers.any()
            && let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab)
            && !view_state.staging_well.has_text_focus()
            && !view_state.branch_sidebar.has_text_focus()
        {
            let path = repo_tab.repo.git_command_dir();
            open_terminal_at(&path.to_string_lossy(), "repo", &mut self.toast_manager);
            return true;
        }

        false
    }
}
