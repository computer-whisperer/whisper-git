use super::*;

impl App {
    pub(crate) fn handle_window_resized(&mut self) {
        self.resize_debounce = Some(Instant::now());
        if let Some(state) = &self.state {
            state.window.request_redraw();
        }
    }

    pub(crate) fn handle_scale_factor_changed(&mut self, scale_factor: f64) {
        let Some(state) = &mut self.state else { return };

        state.scale_factor = scale_factor;
        // Always update render scale immediately — the SDF atlas still works
        // at different scales, just with slightly lower quality until the
        // new atlas is ready.
        state.text_renderer.set_render_scale(scale_factor);
        state.bold_text_renderer.set_render_scale(scale_factor);

        let atlas_build_scale = state.text_renderer.atlas_build_scale() as f64;
        let scale_ratio = if atlas_build_scale > 0.0 {
            scale_factor / atlas_build_scale
        } else {
            1.0
        };
        let needs_rebuild = scale_ratio >= TEXT_REBUILD_SCALE_UP_RATIO
            || scale_ratio <= TEXT_REBUILD_SCALE_DOWN_RATIO;
        if needs_rebuild {
            if std::env::var_os("WHISPER_TEXT_DIAG").is_some() {
                eprintln!(
                    "text_diag scale_change: rebuilding atlas from {:.2} -> {:.2} (ratio {:.3})",
                    atlas_build_scale, scale_factor, scale_ratio
                );
            }
            // Spawn atlas rebuild on background thread to avoid blocking
            // the event loop (which causes Wayland disconnects).
            let cba = state.ctx.command_buffer_allocator.clone();
            let queue = state.ctx.queue.clone();
            let mem = state.ctx.memory_allocator.clone();
            let rp = state.surface.render_pass.clone();
            let dev = state.ctx.device.clone();
            let proxy = self.proxy.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            self.text_rebuild_receiver = Some(rx);
            std::thread::spawn(move || {
                match build_text_renderers(cba, queue, mem, rp, dev, scale_factor, scale_factor) {
                    Ok(renderers) => {
                        let _ = tx.send(renderers);
                    }
                    Err(e) => {
                        eprintln!("Failed to rebuild text atlases: {e:?}");
                    }
                }
                let _ = proxy.send_event(());
            });
        }
        for (repo_tab, view_state) in &mut self.tabs {
            view_state
                .commit_graph_view
                .sync_metrics(&state.text_renderer);
            view_state
                .commit_graph_view
                .update_layout(&repo_tab.commits);
            view_state.branch_sidebar.sync_metrics(&state.text_renderer);
            view_state.staging_well.set_scale(scale_factor as f32);
        }
        state.surface.needs_recreate = true;
        state.window.request_redraw();
    }

    pub(crate) fn handle_redraw_requested(&mut self, event_loop: &ActiveEventLoop) {
        self.last_frame_time = Instant::now();
        let frame_diag = std::env::var_os("WHISPER_FRAME_DIAG").is_some();
        let frame_t0 = Instant::now();

        // Poll background text renderer rebuild (HiDPI monitor switch)
        if let Some(ref rx) = self.text_rebuild_receiver
            && let Ok((text, bold)) = rx.try_recv()
        {
            self.text_rebuild_receiver = None;
            if let Some(state) = &mut self.state {
                state.text_renderer = text;
                state.bold_text_renderer = bold;
                // Re-sync all tab metrics with the new atlas
                for (repo_tab, view_state) in &mut self.tabs {
                    view_state
                        .commit_graph_view
                        .sync_metrics(&state.text_renderer);
                    view_state
                        .commit_graph_view
                        .update_layout(&repo_tab.commits);
                    view_state.branch_sidebar.sync_metrics(&state.text_renderer);
                }
                state.window.request_redraw();
            } else {
                return;
            }
        }

        // Poll async diff stats FIRST — apply completed results before
        // watcher or remote ops can orphan the receiver with a new one
        self.poll_diff_stats();
        // Re-launch diff stats if the previous receiver was orphaned
        self.ensure_diff_stats();
        // Poll background status refresh
        self.poll_status();
        // Poll background repo state refresh
        let t = Instant::now();
        self.poll_repo_state();
        if frame_diag {
            let d = t.elapsed();
            if d.as_millis() > 0 {
                eprintln!(
                    "[frame_diag] poll_repo_state: {:.1}ms",
                    d.as_secs_f64() * 1000.0
                );
            }
        }
        // Poll per-entity dirty check results (submodule/worktree)
        self.poll_dirty_checks();
        // Poll asynchronous watcher initialization (repo open / submodule navigation).
        self.poll_watcher_init();
        // Poll filesystem watcher for external changes
        self.poll_watcher();
        // Poll background remote operations
        self.poll_remote_ops();
        // Poll AI commit message generation
        self.poll_ai_commit();
        // Finalize diagnostic reload if both async results have arrived
        if self.diagnostic_before.is_some() {
            self.finalize_diagnostic_reload();
        }
        // Process any pending messages
        let t = Instant::now();
        self.process_messages();
        if frame_diag {
            let d = t.elapsed();
            if d.as_millis() > 0 {
                eprintln!(
                    "[frame_diag] process_messages: {:.1}ms",
                    d.as_secs_f64() * 1000.0
                );
            }
        }
        // Check if staging well requested an immediate status refresh (e.g., worktree switch)
        if let Some((_rt, vs)) = self.tabs.get_mut(self.active_tab)
            && vs.staging_well.status_refresh_needed
        {
            self.status_dirty = true;
            vs.staging_well.status_refresh_needed = false;
        }
        self.poll_status_refresh_timer();
        self.poll_ref_reconciliation();
        self.poll_ci_refresh();

        self.poll_repo_dialog();
        self.poll_clone_dialog();
        self.poll_clone_receiver();
        self.poll_open_receiver(frame_diag);
        self.poll_welcome_view();

        // Debounce swapchain recreation during rapid resizes (e.g. KDE
        // animated window geometry changes).  Render at the old swapchain
        // size until resizes settle, then recreate once.
        if let Some(last_resize) = self.resize_debounce
            && last_resize.elapsed() >= Duration::from_millis(100)
        {
            if let Some(state) = &mut self.state {
                state.surface.needs_recreate = true;
            }
            self.resize_debounce = None;
        }

        let t = Instant::now();
        match draw_frame(self) {
            Ok(()) => {
                self.consecutive_draw_errors = 0;
            }
            Err(e) => {
                self.consecutive_draw_errors += 1;
                crash_log::breadcrumb(format!("draw_frame error: {e:?}"));
                eprintln!("Draw error (#{}):{e:?}", self.consecutive_draw_errors);
            }
        }
        if frame_diag {
            let d = t.elapsed();
            if d.as_millis() > 0 {
                eprintln!("[frame_diag] draw_frame: {:.1}ms", d.as_secs_f64() * 1000.0);
            }
        }

        self.last_completed_frame = Instant::now();

        if frame_diag {
            let total = frame_t0.elapsed();
            if total.as_millis() > 2 {
                eprintln!(
                    "[frame_diag] === total frame: {:.1}ms ===",
                    total.as_secs_f64() * 1000.0
                );
            }
        }

        // Screenshot mode
        let screenshot_path = self.cli_args.screenshot.clone();
        if let Some(path) = screenshot_path {
            let has_state = self.cli_args.screenshot_state.is_some();
            let capture_frame = if has_state { 4 } else { 3 };
            let Some(frame) = self.state.as_ref().map(|state| state.frame_count) else {
                return;
            };

            // Apply injected state one frame before capture
            if frame == 3 && has_state {
                apply_screenshot_state(self);
            }

            if frame == capture_frame {
                let screenshot_size = self.cli_args.screenshot_size;
                let result = if let Some((width, height)) = screenshot_size {
                    capture_screenshot_offscreen(self, width, height)
                } else {
                    capture_screenshot(self)
                };
                match result {
                    Ok(img) => {
                        if let Err(e) = img.save(&path) {
                            eprintln!("Failed to save screenshot: {e}");
                        } else {
                            println!("Screenshot saved to: {}", path.display());
                        }
                    }
                    Err(e) => eprintln!("Failed to capture screenshot: {e:?}"),
                }
                event_loop.exit();
                return;
            }
            // Screenshot mode needs continuous redraws to reach capture frame
            if let Some(state) = &self.state {
                state.window.request_redraw();
            }
        }
    }

    pub(crate) fn handle_dropped_file(&mut self, path: PathBuf) {
        crash_log::breadcrumb(format!("dropped file: {}", path.display()));
        // Drops can land on a repo workdir or a file inside it; discover walks
        // up to find the .git so both cases work. Multi-file drops fire this
        // handler once per path, opening one tab per dropped repo.
        let repo_path = match git2::Repository::discover(&path) {
            Ok(repo) => repo
                .workdir()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| path.clone()),
            Err(_) => {
                self.toast_manager.push(
                    format!("Not a git repository: {}", path.display()),
                    ToastSeverity::Error,
                );
                return;
            }
        };
        let path_str = repo_path.to_string_lossy().to_string();
        if let Err(e) = self.config.add_recent_repo(&path_str) {
            self.toast_manager.push(e, ToastSeverity::Error);
        }
        self.open_repo_tab(repo_path);
        if let Some(state) = &self.state {
            state.window.request_redraw();
        }
    }

    pub(crate) fn handle_input_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        win_event: &WindowEvent,
    ) {
        // Convert winit event to our InputEvent (brief mutable borrow)
        let input_event = {
            let Some(state) = &mut self.state else { return };
            state.input_state.handle_window_event(win_event)
        };

        if let Some(input_event) = input_event {
            self.handle_input_event(event_loop, &input_event);
            // Any user input should trigger a visual update
            if let Some(state) = &self.state {
                state.window.request_redraw();
            }
        }
    }
}
