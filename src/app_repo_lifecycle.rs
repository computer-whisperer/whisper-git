use super::*;

impl App {
    /// Open a new repo and add it as a tab.
    pub(crate) fn start_clone(&mut self, url: String, dest: PathBuf, bare: bool) {
        if self.clone_receiver.is_some() {
            self.toast_manager.push(
                "A clone is already in progress".to_string(),
                ToastSeverity::Info,
            );
            return;
        }
        let dest_display = dest.display().to_string();
        let label = if bare { "Bare cloning" } else { "Cloning" };
        self.toast_manager.push(
            format!("{label} into {dest_display}..."),
            ToastSeverity::Info,
        );
        let (tx, rx) = std::sync::mpsc::channel::<Result<PathBuf, String>>();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let mut args = vec!["clone"];
            if bare {
                args.push("--bare");
            }
            args.push(&url);
            let dest_str = dest.to_string_lossy().to_string();
            args.push(&dest_str);
            let result = std::process::Command::new("git")
                .args(&args)
                .env("GIT_TERMINAL_PROMPT", "0")
                .output();
            let clone_result = match result {
                Ok(output) if output.status.success() => Ok(dest),
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    Err(stderr)
                }
                Err(e) => Err(format!("Failed to run git clone: {e}")),
            };
            let _ = tx.send(clone_result);
            let _ = proxy.send_event(());
        });
        self.clone_receiver = Some((rx, Instant::now()));
    }

    /// Spawn a background thread to open a repo, avoiding main-thread stalls
    /// that can cause Wayland disconnects.
    pub(crate) fn open_repo_tab(&mut self, path: PathBuf) {
        crash_log::breadcrumb(format!("open_repo_tab: {}", path.display()));
        if self.open_receiver.is_some() {
            self.toast_manager.push(
                "A repo is already being opened".to_string(),
                ToastSeverity::Info,
            );
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = GitRepo::open(&path)
                .map(|repo| {
                    let name = repo.repo_name();
                    (repo, name)
                })
                .map_err(|e| format!("{e}"));
            let _ = tx.send(result);
            let _ = proxy.send_event(());
        });
        self.open_receiver = Some(rx);
    }

    /// Finish creating a tab after the background GitRepo::open completes.
    pub(crate) fn finish_open_repo_tab(&mut self, repo: GitRepo, name: String) {
        self.tab_bar.add_tab(name.clone());
        let mut view_state = TabViewState::new();
        let tab_id = self.next_tab_id;
        self.next_tab_id = self.next_tab_id.saturating_add(1);

        let mut repo_tab = RepoTab {
            id: tab_id,
            repo,
            commits: Vec::new(),
            name,
        };

        if let Some(ref render_state) = self.state {
            view_state.commit_graph_view.row_scale = self.settings_dialog.row_scale;
            view_state.commit_graph_view.abbreviate_worktree_names =
                self.settings_dialog.abbreviate_worktree_names;
            view_state.commit_graph_view.time_spacing_strength =
                self.settings_dialog.time_spacing_strength;
            view_state.commit_graph_view.fast_scroll = self.config.fast_scroll;
            view_state.commit_graph_view.ratchet_scroll = self.config.ratchet_scroll;
            view_state.repo_state_receiver = Some(init_tab_view(
                &mut repo_tab,
                &mut view_state,
                &render_state.text_renderer,
                render_state.scale_factor as f32,
                self.config.show_orphaned_commits,
                &self.proxy,
            ));
        }

        trigger_ci_fetch(&self.config, &repo_tab, &mut view_state, &self.proxy);
        self.tabs.push((repo_tab, view_state));
        let new_idx = self.tabs.len() - 1;
        self.active_tab = new_idx;
        self.tab_bar.set_active(new_idx);
        self.toast_manager.push(
            format!("Opened {}", self.tabs[new_idx].0.name),
            ToastSeverity::Success,
        );
    }

    pub(crate) fn poll_repo_dialog(&mut self) {
        self.repo_dialog.poll_picker();

        if let Some(action) = self.repo_dialog.take_action() {
            match action {
                RepoDialogAction::Open(path) => {
                    let path_str = path.to_string_lossy().to_string();
                    if let Err(e) = self.config.add_recent_repo(&path_str) {
                        self.toast_manager.push(e, ToastSeverity::Error);
                    }
                    self.open_repo_tab(path);
                }
                RepoDialogAction::Cancel => {}
            }
        }
    }

    pub(crate) fn poll_clone_dialog(&mut self) {
        self.clone_dialog.poll();

        if let Some(action) = self.clone_dialog.take_action() {
            match action {
                CloneDialogAction::Clone { url, dest, bare } => {
                    self.start_clone(url, dest, bare);
                }
                CloneDialogAction::Cancel => {}
            }
        }
    }

    pub(crate) fn poll_clone_receiver(&mut self) {
        if let Some((ref rx, _started)) = self.clone_receiver {
            match rx.try_recv() {
                Ok(Ok(dest)) => {
                    self.clone_receiver = None;
                    let dest_str = dest.to_string_lossy().to_string();
                    self.toast_manager
                        .push(format!("Cloned into {}", dest_str), ToastSeverity::Success);
                    if let Err(e) = self.config.add_recent_repo(&dest_str) {
                        self.toast_manager.push(e, ToastSeverity::Error);
                    }
                    self.open_repo_tab(dest);
                }
                Ok(Err(err)) => {
                    self.clone_receiver = None;
                    self.toast_manager.push(
                        format!("Clone failed: {}", err.trim()),
                        ToastSeverity::Error,
                    );
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.clone_receiver = None;
                    self.toast_manager.push(
                        "Clone failed: background thread terminated".to_string(),
                        ToastSeverity::Error,
                    );
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    pub(crate) fn poll_open_receiver(&mut self, frame_diag: bool) {
        if let Some(ref rx) = self.open_receiver {
            match rx.try_recv() {
                Ok(Ok((repo, name))) => {
                    self.open_receiver = None;
                    let t = Instant::now();
                    self.finish_open_repo_tab(repo, name);
                    if frame_diag {
                        eprintln!(
                            "[frame_diag] finish_open_repo_tab: {:.1}ms",
                            t.elapsed().as_secs_f64() * 1000.0
                        );
                    }
                }
                Ok(Err(err)) => {
                    self.open_receiver = None;
                    self.toast_manager
                        .push(format!("Failed to open: {}", err), ToastSeverity::Error);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.open_receiver = None;
                    self.toast_manager.push(
                        "Failed to open: background thread terminated".to_string(),
                        ToastSeverity::Error,
                    );
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
    }
}
