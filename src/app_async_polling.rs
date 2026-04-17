use super::*;

impl App {
    pub(crate) fn refresh_status(&mut self) {
        self.refresh_status_for_tab(self.active_tab);
    }

    pub(crate) fn refresh_status_for_tab(&mut self, tab_idx: usize) {
        let Some((repo_tab, view_state)) = self.tabs.get(tab_idx) else {
            return;
        };
        let repo = &repo_tab.repo;
        let repo_context_path = repo
            .workdir()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| repo.git_dir().to_path_buf());
        let is_bare = repo.is_effectively_bare();

        // Determine staging repo context path (selected worktree or same as main)
        let staging_context_path = view_state
            .worktree_state
            .staging_repo()
            .map(|r| {
                r.workdir()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| r.git_dir().to_path_buf())
            })
            .or_else(|| Some(repo_context_path.clone()));

        if let Some((_, view_state)) = self.tabs.get_mut(tab_idx) {
            view_state.status_receiver = Some(spawn_status_refresh(
                repo_context_path,
                staging_context_path,
                is_bare,
                self.proxy.clone(),
            ));
        }
    }

    pub(crate) fn poll_status(&mut self) {
        for tab_idx in 0..self.tabs.len() {
            let poll = {
                let (_, view_state) = &mut self.tabs[tab_idx];
                poll_slot(&mut view_state.status_receiver)
            };
            match poll {
                ReceiverPoll::Ready(result) => {
                    let (repo_tab, view_state) = &mut self.tabs[tab_idx];
                    apply_status_result(result, repo_tab, view_state);
                }
                ReceiverPoll::Disconnected | ReceiverPoll::Pending => {}
            }
        }
    }

    pub(crate) fn poll_ai_commit(&mut self) {
        let (rx, started) = match self.ai_commit_receiver {
            Some(ref inner) => inner,
            None => return,
        };
        let started = *started;

        match rx.try_recv() {
            Ok(Ok(response)) => {
                self.ai_commit_receiver = None;
                if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                    view_state.staging_well.ai_generating = false;
                    view_state
                        .staging_well
                        .subject_input
                        .set_text(&response.subject);
                    view_state.staging_well.body_area.set_text(&response.body);
                }
                self.toast_manager
                    .push("Commit message generated".to_string(), ToastSeverity::Success);
            }
            Ok(Err(err)) => {
                self.ai_commit_receiver = None;
                if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                    view_state.staging_well.ai_generating = false;
                }
                self.toast_manager.push(
                    format!("AI generation failed: {}", err),
                    ToastSeverity::Error,
                );
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.ai_commit_receiver = None;
                if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                    view_state.staging_well.ai_generating = false;
                }
                self.toast_manager.push(
                    "AI generation failed: thread terminated".to_string(),
                    ToastSeverity::Error,
                );
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if started.elapsed().as_secs() >= 30 {
                    self.ai_commit_receiver = None;
                    if let Some((_, view_state)) = self.tabs.get_mut(self.active_tab) {
                        view_state.staging_well.ai_generating = false;
                    }
                    self.toast_manager
                        .push("AI generation timed out".to_string(), ToastSeverity::Error);
                }
            }
        }
    }

    /// Spawn an async repo state refresh for the active tab.
    pub(crate) fn trigger_repo_state_refresh(&mut self) {
        self.trigger_repo_state_refresh_for_tab(self.active_tab);
    }

    pub(crate) fn trigger_repo_state_refresh_for_tab(&mut self, tab_idx: usize) {
        let Some((repo_tab, view_state)) = self.tabs.get(tab_idx) else {
            return;
        };
        let repo_context_path = repo_tab
            .repo
            .workdir()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| repo_tab.repo.git_dir().to_path_buf());
        let staging_context_path = view_state
            .worktree_state
            .staging_repo()
            .map(|r| {
                r.workdir()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| r.git_dir().to_path_buf())
            })
            .or_else(|| Some(repo_context_path.clone()));

        if let Some((_, view_state)) = self.tabs.get_mut(tab_idx) {
            view_state.repo_state_receiver = Some(spawn_repo_state_refresh(
                repo_context_path,
                staging_context_path,
                self.config.show_orphaned_commits,
                self.proxy.clone(),
            ));
        }
    }

    pub(crate) fn poll_repo_state(&mut self) {
        for tab_idx in 0..self.tabs.len() {
            let poll = {
                let (_, view_state) = &mut self.tabs[tab_idx];
                poll_slot(&mut view_state.repo_state_receiver)
            };
            match poll {
                ReceiverPoll::Ready(result) => {
                    crash_log::breadcrumb(format!(
                        "apply_repo_state(tab={}): {} commits, {} worktrees",
                        tab_idx,
                        result.commits.len(),
                        result.worktrees.len()
                    ));
                    let (repo_tab, view_state) = &mut self.tabs[tab_idx];
                    let rx = apply_repo_state_result(
                        result,
                        repo_tab,
                        view_state,
                        &mut self.toast_manager,
                        &self.proxy,
                    );
                    if rx.is_some() {
                        view_state.diff_stats_receiver = rx;
                    }
                    // Update watcher paths in case worktree structure changed
                    let common_dir = repo_tab.repo.common_dir().to_path_buf();
                    if let Some(ref mut w) = view_state.watcher {
                        w.update_worktree_watches(
                            &view_state.worktree_state.worktrees,
                            &common_dir,
                        );
                    }
                    // Kick off per-entity dirty checks for submodules and worktrees
                    let repo_workdir = repo_tab.repo.workdir().map(|p| p.to_path_buf());
                    self.dirty_checks_in_flight += spawn_dirty_checks(
                        repo_tab.id,
                        &view_state.staging_well.submodules,
                        &view_state.worktree_state.worktrees,
                        repo_workdir,
                        &self.dirty_check_tx,
                        &self.proxy,
                    );
                }
                ReceiverPoll::Disconnected | ReceiverPoll::Pending => {}
            }
        }
    }

    /// Poll per-entity dirty check results and apply them individually.
    pub(crate) fn poll_dirty_checks(&mut self) {
        if self.dirty_checks_in_flight == 0 {
            return;
        }
        loop {
            match self.dirty_check_rx.try_recv() {
                Ok(result) => {
                    self.dirty_checks_in_flight = self.dirty_checks_in_flight.saturating_sub(1);
                    let target_tab_id = result.tab_id();
                    if let Some((repo_tab, view_state)) = self
                        .tabs
                        .iter_mut()
                        .find(|(repo_tab, _)| repo_tab.id == target_tab_id)
                    {
                        apply_dirty_check_result(result, repo_tab, view_state);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // Channel closed — should never happen since we hold the sender
                    self.dirty_checks_in_flight = 0;
                    break;
                }
            }
        }
    }

    /// Re-launch async diff stats for any commits still missing stats.
    /// Runs every frame so orphaned receivers are quickly replaced.
    pub(crate) fn ensure_diff_stats(&mut self) {
        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        if view_state.diff_stats_receiver.is_some() {
            return; // computation already in progress
        }
        let repo = &repo_tab.repo;
        let needs_stats: Vec<Oid> = repo_tab
            .commits
            .iter()
            .filter(|c| !c.is_synthetic && c.insertions == 0 && c.deletions == 0)
            .map(|c| c.id)
            .collect();
        if !needs_stats.is_empty() {
            view_state.diff_stats_receiver =
                Some(repo.compute_diff_stats_async(needs_stats, self.proxy.clone()));
        }
    }

    pub(crate) fn poll_watcher_init(&mut self) {
        for tab_idx in 0..self.tabs.len() {
            let poll = {
                let (_, view_state) = &mut self.tabs[tab_idx];
                poll_slot(&mut view_state.watcher_init_receiver)
            };

            match poll {
                ReceiverPoll::Ready(Ok((watcher, watcher_rx))) => {
                    let (_, view_state) = &mut self.tabs[tab_idx];
                    view_state.watcher = Some(watcher);
                    view_state.watcher_rx = Some(watcher_rx);
                }
                ReceiverPoll::Ready(Err(err)) => {
                    let (_, view_state) = &mut self.tabs[tab_idx];
                    view_state.watcher = None;
                    view_state.watcher_rx = None;
                    self.toast_manager.push(
                        format!("Filesystem watcher failed: {}", err),
                        ToastSeverity::Error,
                    );
                }
                ReceiverPoll::Disconnected => {
                    let (_, view_state) = &mut self.tabs[tab_idx];
                    view_state.watcher = None;
                    view_state.watcher_rx = None;
                    self.toast_manager.push(
                        "Filesystem watcher failed: background thread terminated".to_string(),
                        ToastSeverity::Error,
                    );
                }
                ReceiverPoll::Pending => {}
            }
        }
    }

    pub(crate) fn poll_watcher(&mut self) {
        for tab_idx in 0..self.tabs.len() {
            let max_kind = {
                let (_, view_state) = &mut self.tabs[tab_idx];
                let Some(ref rx) = view_state.watcher_rx else {
                    continue;
                };
                // Drain all pending signals, track the highest-priority kind
                let mut max_kind: Option<FsChangeKind> = None;
                while let Ok(kind) = rx.try_recv() {
                    max_kind = Some(match max_kind {
                        Some(prev) => {
                            if kind.priority() > prev.priority() {
                                kind
                            } else {
                                prev
                            }
                        }
                        None => kind,
                    });
                }
                max_kind
            };

            match max_kind {
                Some(FsChangeKind::WorkingTree) => {
                    if tab_idx == self.active_tab {
                        self.status_dirty = true;
                    } else {
                        self.refresh_status_for_tab(tab_idx);
                    }
                    let (repo_tab, view_state) = &mut self.tabs[tab_idx];
                    // Re-check worktree dirty state (file may have changed in a worktree)
                    self.dirty_checks_in_flight += spawn_dirty_checks(
                        repo_tab.id,
                        &[], // skip submodules for working tree changes
                        &view_state.worktree_state.worktrees,
                        repo_tab.repo.workdir().map(|p| p.to_path_buf()),
                        &self.dirty_check_tx,
                        &self.proxy,
                    );
                }
                Some(FsChangeKind::GitMetadata) => {
                    if tab_idx == self.active_tab {
                        self.status_dirty = true;
                    } else {
                        self.refresh_status_for_tab(tab_idx);
                    }
                    {
                        let (repo_tab, view_state) = &mut self.tabs[tab_idx];
                        // Force-reopen repo handles to bypass libgit2 refdb cache
                        let _ = repo_tab.repo.reopen();
                        for wt_repo in view_state.worktree_state.repo_cache.values_mut() {
                            let _ = wt_repo.reopen();
                        }
                    }
                    self.trigger_repo_state_refresh_for_tab(tab_idx);
                }
                Some(FsChangeKind::WorktreeStructure) => {
                    if tab_idx == self.active_tab {
                        self.status_dirty = true;
                    } else {
                        self.refresh_status_for_tab(tab_idx);
                    }
                    {
                        let (repo_tab, view_state) = &mut self.tabs[tab_idx];
                        // Force-reopen repo handles to bypass libgit2 refdb cache
                        let _ = repo_tab.repo.reopen();
                        for wt_repo in view_state.worktree_state.repo_cache.values_mut() {
                            let _ = wt_repo.reopen();
                        }
                    }
                    self.trigger_repo_state_refresh_for_tab(tab_idx);
                    // Note: watcher path updates will happen when poll_repo_state applies results
                }
                None => {}
            }
        }
    }

    pub(crate) fn poll_diff_stats(&mut self) {
        for tab_idx in 0..self.tabs.len() {
            let poll = {
                let (_, view_state) = &mut self.tabs[tab_idx];
                poll_slot(&mut view_state.diff_stats_receiver)
            };
            match poll {
                ReceiverPoll::Ready(stats) => {
                    let (repo_tab, _) = &mut self.tabs[tab_idx];
                    for (oid, ins, del) in stats {
                        if let Some(commit) = repo_tab.commits.iter_mut().find(|c| c.id == oid) {
                            commit.insertions = ins;
                            commit.deletions = del;
                        }
                    }
                }
                ReceiverPoll::Disconnected | ReceiverPoll::Pending => {}
            }
        }
    }

    pub(crate) fn poll_remote_ops(&mut self) {
        for tab_idx in 0..self.tabs.len() {
            self.poll_remote_ops_for_tab(tab_idx);
        }
    }

    pub(crate) fn poll_remote_ops_for_tab(&mut self, tab_idx: usize) {
        use std::sync::mpsc::TryRecvError;

        let ci_config = self.config.clone();
        let proxy = self.proxy.clone();
        let now = Instant::now();
        const TIMEOUT_SECS: u64 = 60;

        let mut needs_repo_refresh = false;
        {
            let Some((repo_tab, view_state)) = self.tabs.get_mut(tab_idx) else {
                return;
            };

            // Helper: handle the result of polling a fetch/pull/push op.
            // On success, shows a toast and sets needs_repo_refresh flag.
            macro_rules! handle_poll {
                ($op_name:expr, $past_tense:expr, $receiver:expr, $header_flag:expr, $timeout_idx:expr) => {
                    match poll_remote_op(
                        $receiver,
                        $header_flag,
                        &mut view_state.showed_timeout_toast[$timeout_idx],
                        $op_name,
                        now,
                        TIMEOUT_SECS,
                    ) {
                        AsyncOpPoll::Success(remote) => {
                            self.toast_manager.push(
                                format!("{} {}", $past_tense, remote),
                                ToastSeverity::Success,
                            );
                            needs_repo_refresh = true;
                            // Refresh CI status after remote ops
                            trigger_ci_fetch(&ci_config, repo_tab, view_state, &proxy);
                        }
                        AsyncOpPoll::Failed(summary, raw_stderr) => {
                            self.error_dialog.show(
                                &format!("{} Failed", $op_name),
                                &summary,
                                &raw_stderr,
                            );
                            self.interrupted_modal = self.active_modal.take();
                            self.active_modal = Some(ActiveModal::Error);
                            // Refresh even on failure: a failed pull still fetches refs,
                            // a failed merge may leave the repo in a new state, etc.
                            needs_repo_refresh = true;
                        }
                        AsyncOpPoll::Disconnected => {
                            self.error_dialog.show(
                                &format!("{} Failed", $op_name),
                                &format!(
                                    "{} failed: the background thread terminated unexpectedly.",
                                    $op_name
                                ),
                                "",
                            );
                            self.interrupted_modal = self.active_modal.take();
                            self.active_modal = Some(ActiveModal::Error);
                        }
                        AsyncOpPoll::Timeout => {
                            self.toast_manager.push(
                                format!("{} still running...", $op_name),
                                ToastSeverity::Info,
                            );
                        }
                        AsyncOpPoll::Pending => {}
                    }
                };
            }

            handle_poll!(
                "Fetch",
                "Fetched from",
                &mut view_state.fetch_receiver,
                &mut view_state.header_bar.fetching,
                0
            );
            handle_poll!(
                "Pull",
                "Pulled from",
                &mut view_state.pull_receiver,
                &mut view_state.header_bar.pulling,
                1
            );
            let was_pushing = view_state.header_bar.pushing;
            handle_poll!(
                "Push",
                "Pushed to",
                &mut view_state.push_receiver,
                &mut view_state.header_bar.pushing,
                2
            );
            if was_pushing && !view_state.header_bar.pushing {
                view_state.last_push_time = Some(Instant::now());
            }

            // Poll CI status receivers (one per provider)
            view_state.ci_receivers.retain(|rx| match rx.try_recv() {
                Ok(result) => {
                    // Replace any existing result for the same provider
                    view_state
                        .ci_results
                        .retain(|r| r.provider != result.provider);
                    view_state.ci_results.push(result);
                    view_state.ci_results.sort_by_key(|r| r.provider.sort_key());
                    false // remove completed receiver
                }
                _ => true, // keep pending receivers
            });
            // Update per-commit states from merged provider results
            if !view_state.ci_results.is_empty() {
                let fetch = ci::CiFetchResult {
                    providers: view_state.ci_results.clone(),
                };
                view_state.commit_graph_view.ci_commit_rollups =
                    fetch.per_commit_provider_rollups();
            }

            // Poll generic async ops (submodule/worktree operations)
            // This has unique post-success behavior (squash merge toast, worktree/stash refresh)
            // so it's handled separately rather than through the common helper.
            if let Some((ref rx, ref label, started)) = view_state.generic_op_receiver {
                let label = label.clone();
                match rx.try_recv() {
                    Ok(result) => {
                        view_state.generic_op_receiver = None;
                        view_state.showed_timeout_toast[3] = false;
                        if result.success {
                            self.toast_manager
                                .push(format!("{} complete", label), ToastSeverity::Success);
                            // Squash merge doesn't auto-commit: show info toast
                            if label.starts_with("Squash merge") {
                                self.toast_manager.push(
                                    "Squash merge staged. Review and commit when ready."
                                        .to_string(),
                                    ToastSeverity::Info,
                                );
                            }
                            needs_repo_refresh = true;
                        } else {
                            let (msg, _) = git::classify_git_error(&label, &result.error);
                            self.error_dialog.show(
                                &format!("{} Failed", label),
                                &msg,
                                &result.error,
                            );
                            self.interrupted_modal = self.active_modal.take();
                            self.active_modal = Some(ActiveModal::Error);
                        }
                    }
                    Err(TryRecvError::Disconnected) => {
                        view_state.generic_op_receiver = None;
                        view_state.showed_timeout_toast[3] = false;
                        self.error_dialog.show(
                            &format!("{} Failed", label),
                            &format!(
                                "{} failed: the background thread terminated unexpectedly.",
                                label
                            ),
                            "",
                        );
                        self.interrupted_modal = self.active_modal.take();
                        self.active_modal = Some(ActiveModal::Error);
                    }
                    Err(TryRecvError::Empty) => {
                        if now.duration_since(started).as_secs() >= TIMEOUT_SECS
                            && !view_state.showed_timeout_toast[3]
                        {
                            view_state.showed_timeout_toast[3] = true;
                            self.toast_manager
                                .push(format!("{} still running...", label), ToastSeverity::Info);
                        }
                    }
                }
            }
        }

        if needs_repo_refresh {
            self.trigger_repo_state_refresh_for_tab(tab_idx);
            if tab_idx == self.active_tab {
                self.status_dirty = true;
            } else {
                self.refresh_status_for_tab(tab_idx);
            }
        }
    }
}
