use super::*;

impl App {
    pub(crate) fn process_messages(&mut self) {
        let tab_count = self.tabs.len();
        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };

        // Extract messages to avoid borrow conflicts
        let messages: Vec<_> = view_state.pending_messages.drain(..).collect();

        if messages.is_empty() {
            return;
        }
        crash_log::breadcrumb(format!("process_messages: {} pending", messages.len()));

        // Partition: submodule navigation vs normal messages
        let (nav_messages, mut normal_messages): (Vec<_>, Vec<_>) =
            messages.into_iter().partition(|msg| {
                matches!(
                    msg,
                    AppMessage::EnterSubmodule(_)
                        | AppMessage::ExitSubmodule
                        | AppMessage::ExitToDepth(_)
                )
            });

        // Handle ShowPullDialog and ShowPushDialog separately (need to access dialogs)
        normal_messages.retain(|msg| {
            if let AppMessage::ShowPullDialog(branch) = msg {
                let repo = &repo_tab.repo;
                let default_remote = repo
                    .default_remote()
                    .unwrap_or_else(|_| "origin".to_string());
                let remote_names = repo.remote_names();
                self.pull_dialog.show(branch, &default_remote, remote_names);
                self.active_modal = Some(ActiveModal::Pull);
                false
            } else if let AppMessage::ShowPushDialog(branch) = msg {
                let repo = &repo_tab.repo;
                let default_remote = repo
                    .default_remote()
                    .unwrap_or_else(|_| "origin".to_string());
                let remote_names = repo.remote_names();
                self.push_dialog.show(branch, &default_remote, remote_names);
                self.active_modal = Some(ActiveModal::Push);
                false
            } else if matches!(msg, AppMessage::AiGenerateCommitMessage) {
                // Inline AI generation (can't call method due to borrow conflict with repo_tab/view_state)
                if self.ai_commit_receiver.is_some() {
                    self.toast_manager.push(
                        "AI generation already in progress".to_string(),
                        ToastSeverity::Info,
                    );
                } else if view_state.staging_well.staged_list.files.is_empty() {
                    self.toast_manager.push(
                        "No staged files — stage changes first".to_string(),
                        ToastSeverity::Info,
                    );
                } else {
                    let repo = &repo_tab.repo;
                    let staging_repo = view_state.worktree_state.staging_repo_or(repo);
                    match staging_repo.staged_diff_text(50_000) {
                        Ok(diff_text) if diff_text.trim().is_empty() => {
                            self.toast_manager
                                .push("Staged diff is empty".to_string(), ToastSeverity::Info);
                        }
                        Ok(diff_text) => {
                            let branch = view_state.current_branch.clone();
                            let provider = self.ai_provider.clone();
                            let provider_name = provider.display_name().to_string();
                            let (tx, rx) = std::sync::mpsc::channel();
                            let ai_proxy = self.proxy.clone();
                            std::thread::spawn(move || {
                                let request = ai::AiRequest { diff_text, branch };
                                let result = provider.generate_commit_message(&request);
                                let _ = tx.send(result);
                                let _ = ai_proxy.send_event(());
                            });
                            self.ai_commit_receiver = Some((rx, Instant::now()));
                            view_state.staging_well.ai_generating = true;
                            self.toast_manager.push(
                                format!("Generating commit message via {}...", provider_name),
                                ToastSeverity::Info,
                            );
                        }
                        Err(e) => {
                            self.toast_manager.push(
                                format!("Failed to read staged diff: {}", e),
                                ToastSeverity::Error,
                            );
                        }
                    }
                }
                false
            } else {
                true
            }
        });

        // Handle submodule navigation first (needs text_renderer from self.state)
        if !nav_messages.is_empty() {
            let mut nav_rx: Option<Receiver<RepoStateResult>> = None;
            if let Some(ref state) = self.state {
                let scale = state.scale_factor as f32;
                for msg in nav_messages {
                    let show_orphans = self.config.show_orphaned_commits;
                    match msg {
                        AppMessage::EnterSubmodule(name) => {
                            if let Some(rx) = enter_submodule(
                                &name,
                                repo_tab,
                                view_state,
                                &state.text_renderer,
                                scale,
                                &mut self.toast_manager,
                                show_orphans,
                                &self.proxy,
                            ) {
                                nav_rx = Some(rx);
                            }
                        }
                        AppMessage::ExitSubmodule => {
                            if let Some(rx) = exit_submodule(
                                repo_tab,
                                view_state,
                                &state.text_renderer,
                                scale,
                                show_orphans,
                                &self.proxy,
                            ) {
                                nav_rx = Some(rx);
                            }
                        }
                        AppMessage::ExitToDepth(depth) => {
                            if let Some(rx) = exit_to_depth(
                                depth,
                                repo_tab,
                                view_state,
                                &state.text_renderer,
                                scale,
                                show_orphans,
                                &self.proxy,
                            ) {
                                nav_rx = Some(rx);
                            }
                        }
                        _ => unreachable!(),
                    }
                }
            }
            if nav_rx.is_some() {
                view_state.repo_state_receiver = nav_rx;
            }
            // Mark status dirty after submodule navigation to refresh staging well
            self.status_dirty = true;
        }

        if normal_messages.is_empty() {
            return;
        }

        let scale = self
            .state
            .as_ref()
            .map(|s| s.scale_factor as f32)
            .unwrap_or(1.0);

        // Compute graph bounds for JumpToWorktreeBranch
        let graph_bounds = if let Some(ref state) = self.state {
            let extent = state.surface.extent();
            let screen_bounds = Rect::from_size(extent[0] as f32, extent[1] as f32);
            let tab_bar_height = if tab_count > 1 {
                TabBar::height(scale)
            } else {
                0.0
            };
            let (_tab_bar_bounds, main_bounds) = screen_bounds.take_top(tab_bar_height);
            let layout = ScreenLayout::compute_with_ratios_and_shortcut(
                main_bounds,
                4.0,
                scale,
                Some(self.sidebar_ratio),
                Some(self.graph_ratio),
                self.shortcut_bar_visible,
            );
            layout.graph
        } else {
            Rect::new(0.0, 0.0, 1920.0, 1080.0)
        };

        let ctx = MessageContext {
            graph_bounds,
            show_orphaned_commits: self.config.show_orphaned_commits,
        };

        let repo = &repo_tab.repo;

        // Any normal message likely changes state, so mark status dirty
        self.status_dirty = true;

        // Resolve staging_repo via direct field access to avoid borrow conflict
        // (repo_cache is immutably borrowed, worktrees is mutably borrowed in msg_view_state)
        let staging_repo: &GitRepo = view_state
            .worktree_state
            .selected_path
            .as_ref()
            .and_then(|p| view_state.worktree_state.repo_cache.get(p))
            .unwrap_or(repo);
        let mut needs_repo_refresh = false;
        for msg in normal_messages {
            let mut msg_view_state = MessageViewState {
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
            handle_app_message(
                msg,
                repo,
                staging_repo,
                &mut repo_tab.commits,
                &mut msg_view_state,
                &mut self.toast_manager,
                &ctx,
            );
            needs_repo_refresh |= msg_view_state.needs_repo_refresh;
        }
        if needs_repo_refresh {
            self.trigger_repo_state_refresh_for_tab(self.active_tab);
        }
    }
}
