use super::*;

impl App {
    /// Handle events for modal dialogs (confirm, branch name, remote, settings,
    /// repo, token, and clone dialogs). Returns true if consumed.
    pub(crate) fn handle_modal_events(
        &mut self,
        input_event: &InputEvent,
        screen_bounds: Rect,
    ) -> bool {
        let Some(modal) = self.active_modal else {
            // No modal active — check non-modal overlays
            return self.handle_overlay_events(input_event, screen_bounds);
        };

        match modal {
            ActiveModal::Confirm => {
                self.confirm_dialog.handle_event(input_event, screen_bounds);
                if let Some(action) = self.confirm_dialog.take_action() {
                    match action {
                        ConfirmDialogAction::Confirm => {
                            if let Some(msg) = self.pending_confirm_action.take()
                                && let Some((_, view_state)) = self.tabs.get_mut(self.active_tab)
                            {
                                view_state.pending_messages.push(msg);
                            }
                        }
                        ConfirmDialogAction::Cancel => {
                            self.pending_confirm_action = None;
                        }
                    }
                    self.close_interrupt_modal();
                }
            }

            ActiveModal::Error => {
                self.error_dialog.handle_event(input_event, screen_bounds);
                if !self.error_dialog.is_visible() {
                    self.close_interrupt_modal();
                }
            }

            ActiveModal::BranchName => {
                let is_tag = self.branch_name_dialog.title().contains("Tag");
                self.branch_name_dialog
                    .handle_event(input_event, screen_bounds);
                if let Some(action) = self.branch_name_dialog.take_action() {
                    match action {
                        BranchNameDialogAction::Create(name, oid) => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                if is_tag {
                                    view_state
                                        .pending_messages
                                        .push(AppMessage::CreateTag(name, oid));
                                } else {
                                    view_state
                                        .pending_messages
                                        .push(AppMessage::CreateBranch(name, oid));
                                }
                            }
                        }
                        BranchNameDialogAction::CreateWorktree(
                            name,
                            source,
                            init_submodules,
                            checkout_lfs,
                        ) => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                view_state.pending_messages.push(AppMessage::CreateWorktree(
                                    name,
                                    source,
                                    init_submodules,
                                    checkout_lfs,
                                ));
                            }
                        }
                        BranchNameDialogAction::Rename(new_name, old_name) => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::RenameBranch(old_name, new_name));
                            }
                        }
                        BranchNameDialogAction::Cancel => {}
                    }
                    self.close_active_modal();
                }
            }

            ActiveModal::Remote => {
                self.remote_dialog.handle_event(input_event, screen_bounds);
                if let Some(action) = self.remote_dialog.take_action() {
                    match action {
                        RemoteDialogAction::AddRemote(name, url) => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::AddRemote(name, url));
                            }
                        }
                        RemoteDialogAction::EditUrl(name, url) => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::SetRemoteUrl(name, url));
                            }
                        }
                        RemoteDialogAction::Rename(old_name, new_name) => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::RenameRemote(old_name, new_name));
                            }
                        }
                        RemoteDialogAction::Cancel => {}
                    }
                    self.close_active_modal();
                }
            }

            ActiveModal::Pull => {
                self.pull_dialog.handle_event(input_event, screen_bounds);
                if let Some(action) = self.pull_dialog.take_action() {
                    match action {
                        PullDialogAction::Confirm {
                            remote,
                            branch,
                            rebase,
                        } => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::PullBranchFrom {
                                        remote,
                                        branch,
                                        rebase,
                                    });
                            }
                        }
                        PullDialogAction::Cancel => {}
                    }
                    self.close_active_modal();
                }
            }

            ActiveModal::Push => {
                self.push_dialog.handle_event(input_event, screen_bounds);
                if let Some(action) = self.push_dialog.take_action() {
                    match action {
                        PushDialogAction::Confirm {
                            local_branch,
                            remote,
                            remote_branch,
                            force,
                        } => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                view_state.pending_messages.push(AppMessage::PushBranchTo {
                                    local_branch,
                                    remote,
                                    remote_branch,
                                    force,
                                });
                            }
                        }
                        PushDialogAction::Cancel => {}
                    }
                    self.close_active_modal();
                }
            }

            ActiveModal::Merge => {
                self.merge_dialog.handle_event(input_event, screen_bounds);
                if let Some(action) = self.merge_dialog.take_action() {
                    match action {
                        MergeDialogAction::Confirm(branch, strategy, message, target_dir) => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                let msg = match strategy {
                                    MergeStrategy::Default => {
                                        AppMessage::MergeBranch(branch, target_dir)
                                    }
                                    MergeStrategy::NoFastForward => {
                                        let commit_msg = message
                                            .unwrap_or_else(|| format!("Merge branch '{}'", branch));
                                        AppMessage::MergeNoFf(branch, commit_msg, target_dir)
                                    }
                                    MergeStrategy::FastForwardOnly => {
                                        AppMessage::MergeFfOnly(branch, target_dir)
                                    }
                                    MergeStrategy::Squash => {
                                        AppMessage::MergeSquash(branch, target_dir)
                                    }
                                };
                                view_state.pending_messages.push(msg);
                            }
                        }
                        MergeDialogAction::Cancel => {}
                    }
                    self.close_active_modal();
                }
            }

            ActiveModal::Rebase => {
                self.rebase_dialog.handle_event(input_event, screen_bounds);
                if let Some(action) = self.rebase_dialog.take_action() {
                    match action {
                        RebaseDialogAction::Confirm(branch, opts, target_dir) => {
                            if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                                view_state.pending_messages.push(
                                    AppMessage::RebaseBranchWithOptions(
                                        branch,
                                        opts.autostash,
                                        opts.rebase_merges,
                                        target_dir,
                                    ),
                                );
                            }
                        }
                        RebaseDialogAction::Cancel => {}
                    }
                    self.close_active_modal();
                }
            }

            ActiveModal::Settings => {
                self.settings_dialog
                    .handle_event(input_event, screen_bounds);
                if let Some(action) = self.settings_dialog.take_action() {
                    match action {
                        SettingsDialogAction::Close => {
                            self.apply_settings_changes();
                            self.close_active_modal();
                        }
                        SettingsDialogAction::ManageTokens => {
                            // Transition: Settings -> TokenManager
                            self.settings_dialog.hide();
                            self.open_token_dialog();
                            // active_modal is now TokenManager (set by open_token_dialog)
                        }
                    }
                }
            }

            ActiveModal::TokenManager => {
                self.token_dialog.handle_event(input_event, screen_bounds);
                for action in self.token_dialog.take_actions() {
                    match action {
                        TokenDialogAction::Close => {
                            // Transition: TokenManager -> Settings
                            self.token_dialog.hide();
                            self.settings_dialog.show();
                            self.active_modal = Some(ActiveModal::Settings);
                        }
                        other => self.handle_token_action(other),
                    }
                }
            }

            ActiveModal::RepoDialog => {
                self.repo_dialog.handle_event(input_event, screen_bounds);
                if !self.repo_dialog.is_visible() {
                    self.active_modal = None;
                }
            }

            ActiveModal::CloneDialog => {
                self.clone_dialog.handle_event(input_event, screen_bounds);
                if !self.clone_dialog.is_visible() {
                    self.active_modal = None;
                }
            }
        }

        true
    }

    /// Handle non-modal overlay events (toasts, context menus).
    pub(crate) fn handle_overlay_events(
        &mut self,
        input_event: &InputEvent,
        screen_bounds: Rect,
    ) -> bool {
        // Toast click-to-dismiss
        if self.toast_manager.handle_event(input_event, screen_bounds) {
            return true;
        }

        // Context menu
        if let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab)
            && view_state.context_menu.is_visible()
        {
            view_state
                .context_menu
                .handle_event(input_event, screen_bounds);
            if let Some(action) = view_state.context_menu.take_action() {
                match action {
                    MenuAction::Selected(action_id) => {
                        handle_context_menu_action(
                            &action_id,
                            view_state,
                            &mut self.toast_manager,
                            &mut self.confirm_dialog,
                            &mut self.branch_name_dialog,
                            &mut self.remote_dialog,
                            &mut self.merge_dialog,
                            &mut self.rebase_dialog,
                            &repo_tab.repo,
                            &mut self.pending_confirm_action,
                            &mut self.active_modal,
                        );
                    }
                }
            }
            return true;
        }

        false
    }

    /// Handle divider drag events (ongoing drag and starting new drags).
    /// Returns true if the event was consumed.
    pub(crate) fn handle_divider_drag(
        &mut self,
        input_event: &InputEvent,
        main_bounds: Rect,
        layout: &ScreenLayout,
    ) -> bool {
        // Handle ongoing drag (MouseMove / MouseUp) before anything else
        if self.divider_drag.is_some() {
            match input_event {
                InputEvent::MouseMove { x, y, .. } => {
                    let drag_kind = self.divider_drag.unwrap();
                    match drag_kind {
                        DividerDrag::SidebarGraph => {
                            let ratio = (*x - main_bounds.x) / main_bounds.width;
                            self.sidebar_ratio = ratio.clamp(0.05, 0.30);
                        }
                        DividerDrag::GraphRight => {
                            let sidebar_w =
                                main_bounds.width * self.sidebar_ratio.clamp(0.05, 0.30);
                            let content_x = main_bounds.x + sidebar_w;
                            let content_w = main_bounds.width - sidebar_w;
                            if content_w > 0.0 {
                                let ratio = (*x - content_x) / content_w;
                                self.graph_ratio = ratio.clamp(0.30, 0.80);
                            }
                        }
                        DividerDrag::StagingPreview => {
                            // Compute pill bar height to get content rect
                            if let Some((_, view_state)) = self.tabs.get(self.active_tab) {
                                let pill_bar_h = view_state
                                    .staging_well
                                    .pill_bar_height(&view_state.current_branch);
                                let (_, content_rect) = layout.right_panel.take_top(pill_bar_h);
                                if content_rect.height > 0.0 {
                                    let ratio = (*y - content_rect.y) / content_rect.height;
                                    self.staging_preview_ratio = ratio.clamp(0.30, 0.70);
                                }
                            }
                        }
                    }
                    if let Some(ref render_state) = self.state {
                        let cursor = match drag_kind {
                            DividerDrag::StagingPreview => CursorIcon::RowResize,
                            _ => CursorIcon::ColResize,
                        };
                        if self.current_cursor != cursor {
                            render_state.window.set_cursor(cursor);
                            self.current_cursor = cursor;
                        }
                    }
                    return true;
                }
                InputEvent::MouseUp { .. } => {
                    self.divider_drag = None;
                    if let Some(ref render_state) = self.state
                        && self.current_cursor != CursorIcon::Default
                    {
                        render_state.window.set_cursor(CursorIcon::Default);
                        self.current_cursor = CursorIcon::Default;
                    }
                    return true;
                }
                _ => {}
            }
        }

        // Start divider drag on MouseDown near divider edges (wide 8px hit zone)
        if let InputEvent::MouseDown {
            button: input::MouseButton::Left,
            x,
            y,
            ..
        } = input_event
        {
            let hit_tolerance = 8.0;

            if *y > layout.shortcut_bar.bottom() {
                let sidebar_edge = layout.sidebar.right();
                if (*x - sidebar_edge).abs() < hit_tolerance {
                    self.divider_drag = Some(DividerDrag::SidebarGraph);
                    return true;
                }

                let graph_edge = layout.graph.right();
                if (*x - graph_edge).abs() < hit_tolerance {
                    self.divider_drag = Some(DividerDrag::GraphRight);
                    return true;
                }

                // Horizontal divider: staging | preview (within right panel, staging mode only)
                if layout.right_panel.contains(*x, *y)
                    && let Some((_, view_state)) = self.tabs.get(self.active_tab)
                    && view_state.right_panel_mode == RightPanelMode::Staging
                {
                    let pill_bar_h = view_state
                        .staging_well
                        .pill_bar_height(&view_state.current_branch);
                    let (_, content_rect) = layout.right_panel.take_top(pill_bar_h);
                    let split_y =
                        content_rect.y + content_rect.height * self.staging_preview_ratio;
                    if (*y - split_y).abs() < hit_tolerance {
                        self.divider_drag = Some(DividerDrag::StagingPreview);
                        return true;
                    }
                }
            }
        }

        false
    }

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

    /// Handle a sidebar action by dispatching to the appropriate pending
    /// message or dialog.
    pub(crate) fn handle_sidebar_action(&mut self, action: SidebarAction) {
        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };

        match action {
            SidebarAction::Checkout(name) => {
                view_state
                    .pending_messages
                    .push(AppMessage::CheckoutBranch(name));
            }
            SidebarAction::CheckoutRemote(remote, branch) => {
                view_state
                    .pending_messages
                    .push(AppMessage::CheckoutRemoteBranch(remote, branch));
            }
            SidebarAction::Delete(name) => {
                self.confirm_dialog
                    .show("Delete Branch", &format!("Delete local branch '{}'?", name));
                self.pending_confirm_action = Some(AppMessage::DeleteBranch(name));
                self.open_interrupt_modal(ActiveModal::Confirm);
            }
            SidebarAction::ApplyStash(index) => {
                view_state
                    .pending_messages
                    .push(AppMessage::StashApply(index));
            }
            SidebarAction::DropStash(index) => {
                self.confirm_dialog.show(
                    "Drop Stash",
                    &format!("Drop stash@{{{}}}? This cannot be undone.", index),
                );
                self.pending_confirm_action = Some(AppMessage::StashDrop(index));
                self.open_interrupt_modal(ActiveModal::Confirm);
            }
            SidebarAction::DeleteTag(name) => {
                self.confirm_dialog
                    .show("Delete Tag", &format!("Delete tag '{}'?", name));
                self.pending_confirm_action = Some(AppMessage::DeleteTag(name));
                self.open_interrupt_modal(ActiveModal::Confirm);
            }
            SidebarAction::SwitchWorktree(wt_name) => {
                view_state.switch_to_worktree_by_name(&wt_name, &repo_tab.repo);
            }
            SidebarAction::JumpToRef(ref_name) => {
                // Look up OID from branch tips or tags
                let oid = view_state
                    .commit_graph_view
                    .branch_tips
                    .iter()
                    .find(|t| t.name == ref_name)
                    .map(|t| t.oid)
                    .or_else(|| {
                        view_state
                            .commit_graph_view
                            .tags
                            .iter()
                            .find(|t| t.name == ref_name)
                            .map(|t| t.oid)
                    });
                if let Some(oid) = oid {
                    view_state
                        .pending_messages
                        .push(AppMessage::JumpToCommit(oid));
                }
            }
        }
    }
}
