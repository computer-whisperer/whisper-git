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
                                        let commit_msg = message.unwrap_or_else(|| {
                                            format!("Merge branch '{}'", branch)
                                        });
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
                    let split_y = content_rect.y + content_rect.height * self.staging_preview_ratio;
                    if (*y - split_y).abs() < hit_tolerance {
                        self.divider_drag = Some(DividerDrag::StagingPreview);
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Dispatch an input event to the appropriate handler.
    pub(crate) fn handle_input_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        input_event: &InputEvent,
    ) {
        let Some(ref state) = self.state else { return };

        // Calculate layout
        let extent = state.surface.extent();
        let screen_bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
        let scale = state.scale_factor as f32;
        let tab_bar_height = if self.tabs.len() > 1 {
            TabBar::height(scale)
        } else {
            0.0
        };
        let (tab_bar_bounds, main_bounds) = screen_bounds.take_top(tab_bar_height);
        let layout = ScreenLayout::compute_with_ratios_and_shortcut(
            main_bounds,
            4.0,
            scale,
            Some(self.sidebar_ratio),
            Some(self.graph_ratio),
            self.shortcut_bar_visible,
        );

        // Modal dialogs consume all events when visible
        if self.handle_modal_events(input_event, screen_bounds) {
            return;
        }

        // Divider drag handling
        if self.handle_divider_drag(input_event, main_bounds, &layout) {
            return;
        }

        // Global keyboard shortcuts
        if self.handle_global_shortcuts(input_event) {
            return;
        }

        // Route to tab bar (if visible)
        if self.tabs.len() > 1
            && self
                .tab_bar
                .handle_event(input_event, tab_bar_bounds)
                .is_consumed()
        {
            if let Some(action) = self.tab_bar.take_action() {
                match action {
                    TabAction::Select(idx) => self.switch_tab(idx),
                    TabAction::Close(idx) => self.close_tab(idx),
                    TabAction::New => {
                        self.repo_dialog.show_with_recent(&self.config.recent_repos);
                        self.active_modal = Some(ActiveModal::RepoDialog);
                    }
                }
            }
            return;
        }

        // Route to active tab's views
        let tab_count = self.tabs.len();
        let Some((_repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };

        // Handle per-tab global keys (except Tab, which is handled after panel routing)
        if let InputEvent::KeyDown { key, .. } = input_event
            && key == &Key::Escape
        {
            if view_state.right_panel_mode == RightPanelMode::Browse {
                view_state.right_panel_mode = RightPanelMode::Staging;
                view_state.commit_detail_view.clear();
                view_state.diff_view.clear();
                view_state.last_diff_commit = None;
            } else if view_state.diff_view.has_content() {
                view_state.diff_view.clear();
                view_state.last_diff_commit = None;
            } else if view_state.submodule_focus.is_some() {
                view_state.pending_messages.push(AppMessage::ExitSubmodule);
            } else {
                event_loop.exit();
            }
            return;
        }

        // Route to branch sidebar
        if view_state
            .branch_sidebar
            .handle_event(input_event, layout.sidebar)
            .is_consumed()
        {
            if matches!(input_event, InputEvent::MouseDown { .. }) {
                view_state.focused_panel = FocusedPanel::Sidebar;
                view_state.branch_sidebar.set_focused(true);
            }
            if let Some(action) = view_state.branch_sidebar.take_action() {
                self.handle_sidebar_action(action);
            }
            return;
        }

        // Route to header bar
        let mut do_reload = false;
        let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        if view_state
            .header_bar
            .handle_event(input_event, layout.header)
            .is_consumed()
        {
            if let Some(action) = view_state.header_bar.take_action() {
                use crate::ui::widgets::HeaderAction;
                match action {
                    HeaderAction::Fetch => {
                        view_state.pending_messages.push(AppMessage::Fetch(None));
                    }
                    HeaderAction::Pull => {
                        let branch = view_state
                            .current_branch_opt()
                            .unwrap_or("HEAD")
                            .to_string();
                        view_state.pending_messages.push(AppMessage::Pull {
                            remote: None,
                            branch,
                        });
                    }
                    HeaderAction::PullRebase => {
                        let branch = view_state
                            .current_branch_opt()
                            .unwrap_or("HEAD")
                            .to_string();
                        view_state.pending_messages.push(AppMessage::PullRebase {
                            remote: None,
                            branch,
                        });
                    }
                    HeaderAction::Push => {
                        let branch = view_state
                            .current_branch_opt()
                            .unwrap_or("HEAD")
                            .to_string();
                        view_state.pending_messages.push(AppMessage::Push {
                            remote: None,
                            branch,
                        });
                    }
                    HeaderAction::Help => {
                        self.shortcut_bar_visible = !self.shortcut_bar_visible;
                        self.config.shortcut_bar_visible = self.shortcut_bar_visible;
                        if let Err(e) = self.config.save() {
                            self.toast_manager.push(e, ToastSeverity::Error);
                        }
                    }
                    HeaderAction::Settings => {
                        self.set_active_modal(ActiveModal::Settings);
                    }
                    HeaderAction::BreadcrumbNav(depth) => {
                        view_state
                            .pending_messages
                            .push(AppMessage::ExitToDepth(depth));
                    }
                    HeaderAction::BreadcrumbClose => {
                        view_state.pending_messages.push(AppMessage::ExitToDepth(0));
                    }
                    HeaderAction::AbortOperation => {
                        view_state.pending_messages.push(AppMessage::AbortOperation);
                    }
                    HeaderAction::Reload => {
                        do_reload = true;
                    }
                    HeaderAction::OpenCiDetails(url) => {
                        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
                    }
                }
            }
            if do_reload {
                self.do_diagnostic_reload();
            }
            return;
        }

        // Route events to right panel content (commit detail + diff view)
        {
            let pill_bar_h = view_state
                .staging_well
                .pill_bar_height(&view_state.current_branch);
            let (_pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);

            // Route to commit detail view when in browse mode
            if view_state.right_panel_mode == RightPanelMode::Browse
                && view_state.commit_detail_view.has_content()
            {
                let (detail_rect, _diff_rect) = content_rect.split_vertical(0.40);
                if view_state
                    .commit_detail_view
                    .handle_event(input_event, detail_rect)
                    .is_consumed()
                {
                    if let Some(action) = view_state.commit_detail_view.take_action() {
                        match action {
                            CommitDetailAction::ViewFileDiff(oid, path) => {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::ViewCommitFileDiff(oid, path));
                            }
                            CommitDetailAction::OpenSubmodule(path_or_name) => {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::EnterSubmodule(path_or_name));
                            }
                        }
                    }
                    return;
                }
            }

            // Route events to diff view if it has content (both modes)
            // Skip keyboard events when staging well has text focus (commit message editing)
            if view_state.diff_view.has_content()
                && !(view_state.staging_well.has_text_focus()
                    && matches!(input_event, InputEvent::KeyDown { .. }))
            {
                let header_h = 28.0 * scale;
                let diff_bounds = match view_state.right_panel_mode {
                    RightPanelMode::Browse if view_state.commit_detail_view.has_content() => {
                        let (_detail_rect, diff_rect) = content_rect.split_vertical(0.40);
                        let (_hdr, body) = diff_rect.take_top(header_h);
                        body
                    }
                    RightPanelMode::Staging => {
                        let (_staging_rect, diff_rect) =
                            content_rect.split_vertical(self.staging_preview_ratio);
                        let (_hdr, body) = diff_rect.take_top(header_h);
                        body
                    }
                    _ => {
                        let (_hdr, body) = content_rect.take_top(header_h);
                        body
                    }
                };
                if view_state
                    .diff_view
                    .handle_event(input_event, diff_bounds)
                    .is_consumed()
                {
                    if let Some(action) = view_state.diff_view.take_action() {
                        match action {
                            DiffAction::Stage(path, hunk_idx) => {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::StageHunk(path, hunk_idx));
                            }
                            DiffAction::Unstage(path, hunk_idx) => {
                                view_state
                                    .pending_messages
                                    .push(AppMessage::UnstageHunk(path, hunk_idx));
                            }
                            DiffAction::Discard(path, hunk_idx) => {
                                self.confirm_dialog.show(
                                    "Discard Hunk",
                                    "Discard this hunk? This cannot be undone.",
                                );
                                self.pending_confirm_action =
                                    Some(AppMessage::DiscardHunk(path, hunk_idx));
                                self.open_interrupt_modal(ActiveModal::Confirm);
                            }
                        }
                    }
                    return;
                }
            }
        }

        // Right-click context menus
        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        if let InputEvent::MouseDown {
            button: input::MouseButton::Right,
            x,
            y,
            ..
        } = input_event
        {
            // Check which panel was right-clicked and show context menu
            if layout.graph.contains(*x, *y) {
                if let Some((items, oid)) = view_state.commit_graph_view.context_menu_items_at(
                    *x,
                    *y,
                    &repo_tab.commits,
                    layout.graph,
                ) {
                    view_state.context_menu_commit = Some(oid);
                    view_state.context_menu.show(items, *x, *y);
                    return;
                }
            } else if layout.sidebar.contains(*x, *y) {
                if let Some(items) = view_state.branch_sidebar.context_menu_items_at(
                    *x,
                    *y,
                    layout.sidebar,
                    &view_state.current_branch,
                ) {
                    view_state.context_menu.show(items, *x, *y);
                    return;
                }
            } else if layout.right_panel.contains(*x, *y) {
                // Check pill bar first
                let pill_bar_h = view_state
                    .staging_well
                    .pill_bar_height(&view_state.current_branch);
                let (pill_rect, _) = layout.right_panel.take_top(pill_bar_h);
                if pill_rect.contains(*x, *y)
                    && let Some(items) = view_state.staging_well.pill_context_menu_at(*x, *y)
                {
                    view_state.context_menu.show(items, *x, *y);
                    return;
                }
                // Then check staging file lists
                if view_state.right_panel_mode == RightPanelMode::Staging {
                    let (_pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);
                    let (staging_rect, _) = content_rect.split_vertical(self.staging_preview_ratio);
                    if let Some(items) =
                        view_state
                            .staging_well
                            .context_menu_items_at(*x, *y, staging_rect)
                    {
                        view_state.context_menu.show(items, *x, *y);
                        return;
                    }
                }
            }
        }

        // Detect clicks on panels to switch focus
        if let InputEvent::MouseDown { x, y, .. } = input_event {
            if layout.right_panel.contains(*x, *y) {
                view_state.focused_panel = FocusedPanel::RightPanel;
            } else if layout.graph.contains(*x, *y) {
                view_state.focused_panel = FocusedPanel::Graph;
            } else if layout.sidebar.contains(*x, *y) {
                view_state.focused_panel = FocusedPanel::Sidebar;
                view_state.branch_sidebar.set_focused(true);
            }
            if view_state.focused_panel != FocusedPanel::Sidebar {
                view_state.branch_sidebar.set_focused(false);
            }
            if view_state.focused_panel != FocusedPanel::RightPanel
                || view_state.right_panel_mode != RightPanelMode::Staging
            {
                view_state.staging_well.unfocus_all();
            }
        }

        // Handle worktree pill bar clicks (before content routing)
        if let InputEvent::MouseDown { .. } = input_event {
            let pill_bar_h = view_state
                .staging_well
                .pill_bar_height(&view_state.current_branch);
            let (pill_rect, _content_rect) = layout.right_panel.take_top(pill_bar_h);
            if view_state
                .staging_well
                .handle_pill_event(input_event, pill_rect)
                .is_consumed()
            {
                if let Some(action) = view_state.staging_well.take_action()
                    && let StagingAction::SwitchWorktree(index) = action
                {
                    view_state.switch_to_worktree(index, &repo_tab.repo);
                }
                return;
            }
        }

        // Route scroll events to the panel under the mouse cursor (hover-based, not focus-based)
        if let InputEvent::Scroll { x, y, .. } = input_event {
            if layout.graph.contains(*x, *y) {
                let prev_selected = view_state.commit_graph_view.selected_commit;
                let response = view_state.commit_graph_view.handle_event(
                    input_event,
                    &repo_tab.commits,
                    layout.graph,
                    view_state.head_oid,
                );
                if view_state.commit_graph_view.selected_commit != prev_selected
                    && let Some(oid) = view_state.commit_graph_view.selected_commit
                    && view_state.last_diff_commit != Some(oid)
                {
                    if let Some(synthetic) = repo_tab
                        .commits
                        .iter()
                        .find(|c| c.id == oid && c.is_synthetic)
                    {
                        // Synthetic row: switch to that worktree if named
                        if let Some(wt_name) = synthetic.synthetic_wt_name.clone() {
                            view_state.switch_to_worktree_by_name(&wt_name, &repo_tab.repo);
                        } else {
                            // Single-worktree: enter staging mode directly
                            view_state.right_panel_mode = RightPanelMode::Staging;
                            view_state.last_diff_commit = None;
                            view_state.commit_detail_view.clear();
                            view_state.diff_view.clear();
                        }
                    } else {
                        view_state
                            .pending_messages
                            .push(AppMessage::SelectedCommit(oid));
                    }
                }
                if let Some(action) = view_state.commit_graph_view.take_action() {
                    view_state.handle_graph_action(action, &repo_tab.repo);
                }
                if response.is_consumed() {
                    return;
                }
            } else if layout.right_panel.contains(*x, *y)
                && view_state.right_panel_mode == RightPanelMode::Staging
            {
                let pill_bar_h = view_state
                    .staging_well
                    .pill_bar_height(&view_state.current_branch);
                let (_pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);
                let (staging_rect, _diff_rect) =
                    content_rect.split_vertical(self.staging_preview_ratio);
                let response = view_state
                    .staging_well
                    .handle_event(input_event, staging_rect);
                if let Some(action) = view_state.staging_well.take_action() {
                    view_state.handle_staging_action(action, &repo_tab.repo);
                }
                if response.is_consumed() {
                    return;
                }
            } else if layout.sidebar.contains(*x, *y) {
                // Sidebar scroll already routed above
            }
            // Scroll events should not fall through to focus-based routing
            return;
        }

        // Route to focused panel
        match view_state.focused_panel {
            FocusedPanel::Graph => {
                let prev_selected = view_state.commit_graph_view.selected_commit;
                let response = view_state.commit_graph_view.handle_event(
                    input_event,
                    &repo_tab.commits,
                    layout.graph,
                    view_state.head_oid,
                );
                if view_state.commit_graph_view.selected_commit != prev_selected
                    && let Some(oid) = view_state.commit_graph_view.selected_commit
                    && view_state.last_diff_commit != Some(oid)
                {
                    if let Some(synthetic) = repo_tab
                        .commits
                        .iter()
                        .find(|c| c.id == oid && c.is_synthetic)
                    {
                        if let Some(wt_name) = synthetic.synthetic_wt_name.clone() {
                            view_state.switch_to_worktree_by_name(&wt_name, &repo_tab.repo);
                        } else {
                            // Single-worktree: enter staging mode directly
                            view_state.right_panel_mode = RightPanelMode::Staging;
                            view_state.last_diff_commit = None;
                            view_state.commit_detail_view.clear();
                            view_state.diff_view.clear();
                        }
                    } else {
                        view_state
                            .pending_messages
                            .push(AppMessage::SelectedCommit(oid));
                    }
                }
                if let Some(action) = view_state.commit_graph_view.take_action() {
                    view_state.handle_graph_action(action, &repo_tab.repo);
                }
                if response.is_consumed() {
                    return;
                }
            }
            FocusedPanel::RightPanel => {
                if view_state.right_panel_mode == RightPanelMode::Staging {
                    let pill_bar_h = view_state
                        .staging_well
                        .pill_bar_height(&view_state.current_branch);
                    let (_pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);
                    let (staging_rect, _diff_rect) =
                        content_rect.split_vertical(self.staging_preview_ratio);
                    let response = view_state
                        .staging_well
                        .handle_event(input_event, staging_rect);

                    if let Some(action) = view_state.staging_well.take_action() {
                        view_state.handle_staging_action(action, &repo_tab.repo);
                    }
                    if response.is_consumed() {
                        return;
                    }
                }
                // Browse mode: diff/detail view events are handled above in pre-routing
            }
            FocusedPanel::Sidebar => {
                // Keyboard events handled by branch_sidebar.handle_event above
            }
        }

        // Tab to cycle panels (only when not consumed by focused panel)
        if let InputEvent::KeyDown { key: Key::Tab, .. } = input_event {
            view_state.focused_panel = match view_state.focused_panel {
                FocusedPanel::Graph => FocusedPanel::RightPanel,
                FocusedPanel::RightPanel => FocusedPanel::Sidebar,
                FocusedPanel::Sidebar => FocusedPanel::Graph,
            };
            view_state
                .branch_sidebar
                .set_focused(view_state.focused_panel == FocusedPanel::Sidebar);
            if view_state.focused_panel != FocusedPanel::RightPanel
                || view_state.right_panel_mode != RightPanelMode::Staging
            {
                view_state.staging_well.unfocus_all();
            }
            return;
        }

        // Update hover states
        if let InputEvent::MouseMove { x, y, .. } = input_event {
            view_state.header_bar.update_hover(*x, *y, layout.header);
            view_state
                .branch_sidebar
                .update_hover(*x, *y, layout.sidebar);
            {
                let pill_bar_h = view_state
                    .staging_well
                    .pill_bar_height(&view_state.current_branch);
                let (_pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);
                let (staging_rect, _diff_rect) =
                    content_rect.split_vertical(self.staging_preview_ratio);
                view_state.staging_well.update_hover(*x, *y, staging_rect);
            }

            if let Some(ref render_state) = self.state {
                if tab_count > 1 {
                    self.tab_bar.update_hover_with_renderer(
                        *x,
                        *y,
                        tab_bar_bounds,
                        &render_state.text_renderer,
                    );
                }

                let cursor = determine_cursor(
                    *x,
                    *y,
                    &layout,
                    view_state,
                    &self.tab_bar,
                    tab_count,
                    self.staging_preview_ratio,
                );
                if self.current_cursor != cursor {
                    render_state.window.set_cursor(cursor);
                    self.current_cursor = cursor;
                }
            }
        }
    }
}

/// Determine which cursor icon to show based on mouse position.
/// Returns resize cursors near divider edges, Pointer over clickable elements,
/// Text cursor over text inputs, Default otherwise.
fn determine_cursor(
    x: f32,
    y: f32,
    layout: &ScreenLayout,
    view_state: &TabViewState,
    tab_bar: &TabBar,
    tab_count: usize,
    staging_preview_ratio: f32,
) -> CursorIcon {
    // Wider hit zone for dividers (8px) makes dragging much easier
    let divider_hit = 8.0;

    // Check divider hover zones (only below shortcut bar)
    if y > layout.shortcut_bar.bottom() {
        // Divider 1: sidebar | graph (vertical)
        let sidebar_edge = layout.sidebar.right();
        if (x - sidebar_edge).abs() < divider_hit {
            return CursorIcon::ColResize;
        }

        // Divider 2: graph | right panel (vertical)
        let graph_edge = layout.graph.right();
        if (x - graph_edge).abs() < divider_hit {
            return CursorIcon::ColResize;
        }

        // Divider 3: staging | preview (horizontal, within right panel, staging mode only)
        if layout.right_panel.contains(x, y)
            && view_state.right_panel_mode == RightPanelMode::Staging
        {
            let pill_bar_h = view_state
                .staging_well
                .pill_bar_height(&view_state.current_branch);
            let (_, content_rect) = layout.right_panel.take_top(pill_bar_h);
            let split_y = content_rect.y + content_rect.height * staging_preview_ratio;
            if (y - split_y).abs() < divider_hit {
                return CursorIcon::RowResize;
            }
        }
    }

    // -- Text cursor: text input fields --

    // Staging area text inputs (subject line, body area) - only in staging mode
    if layout.right_panel.contains(x, y) && view_state.right_panel_mode == RightPanelMode::Staging {
        let pill_bar_h = view_state
            .staging_well
            .pill_bar_height(&view_state.current_branch); // scale already in pill_bar_height
        let (_pill_rect, content_rect) = layout.right_panel.take_top(pill_bar_h);
        let (staging_rect, _diff_rect) = content_rect.split_vertical(staging_preview_ratio);
        let (_, _, subject_bounds, body_bounds, _) =
            view_state.staging_well.compute_regions(staging_rect);
        if subject_bounds.contains(x, y) || body_bounds.contains(x, y) {
            return CursorIcon::Text;
        }
    }

    // Search bar when active (overlays the graph area)
    if view_state.commit_graph_view.search_bar.is_active() && layout.graph.contains(x, y) {
        let scrollbar_width = theme::SCROLLBAR_WIDTH;
        let search_bar_height = 30.0;
        let search_bounds = Rect::new(
            layout.graph.x + 40.0,
            layout.graph.y + 4.0,
            layout.graph.width - 80.0 - scrollbar_width,
            search_bar_height,
        );
        if search_bounds.contains(x, y) {
            return CursorIcon::Text;
        }
    }

    // Sidebar filter bar (show text cursor when hovering the filter input area)
    if layout.sidebar.contains(x, y)
        && view_state
            .branch_sidebar
            .is_over_filter_bar(x, y, layout.sidebar)
    {
        return CursorIcon::Text;
    }

    // -- Pointer cursor: clickable elements --

    // Header bar buttons, breadcrumb links, abort button
    if layout.header.contains(x, y) && view_state.header_bar.is_any_interactive_hovered() {
        return CursorIcon::Pointer;
    }

    // Tab bar tabs, close buttons, new button
    if tab_count > 1 && tab_bar.is_any_hovered() {
        return CursorIcon::Pointer;
    }

    // Sidebar clickable items (branches, tags, stashes, etc.)
    if layout.sidebar.contains(x, y) && view_state.branch_sidebar.is_item_hovered() {
        return CursorIcon::Pointer;
    }

    // Staging well worktree pills (clickable to switch worktree)
    if layout.right_panel.contains(x, y) && view_state.staging_well.is_over_pill(x, y) {
        return CursorIcon::Pointer;
    }

    // Staging well buttons (Stage All, Unstage All, Commit, Amend)
    if layout.right_panel.contains(x, y) && view_state.staging_well.is_any_button_hovered() {
        return CursorIcon::Pointer;
    }

    // Staging well file list items (clickable to select/stage/unstage)
    if layout.right_panel.contains(x, y) && view_state.staging_well.is_file_hovered() {
        return CursorIcon::Pointer;
    }

    // Commit graph worktree/working pills (clickable to switch worktree)
    if layout.graph.contains(x, y) && view_state.commit_graph_view.hovered_pill {
        return CursorIcon::Pointer;
    }

    // Commit graph rows (clickable to select commit)
    if layout.graph.contains(x, y) && view_state.commit_graph_view.hovered_commit.is_some() {
        return CursorIcon::Pointer;
    }

    CursorIcon::Default
}
