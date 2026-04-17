use super::*;

impl App {
    pub(crate) fn new(cli_args: CliArgs, proxy: EventLoopProxy<()>) -> Result<Self> {
        let mut config = Config::load();
        let mut tabs = Vec::new();
        let mut tab_bar = TabBar::new();
        let mut next_tab_id = 1_u64;

        // Determine repo paths to open
        let repo_paths: Vec<PathBuf> = if cli_args.repos.is_empty() {
            vec![PathBuf::from(".")]
        } else {
            cli_args.repos.clone()
        };

        for repo_path in &repo_paths {
            match GitRepo::open(repo_path) {
                Ok(repo) => {
                    let name = repo.repo_name();
                    tab_bar.add_tab(name.clone());
                    tabs.push((
                        RepoTab {
                            id: next_tab_id,
                            repo,
                            commits: Vec::new(),
                            name,
                        },
                        TabViewState::new(),
                    ));
                    next_tab_id = next_tab_id.saturating_add(1);
                }
                Err(e) => {
                    eprintln!("Warning: Could not open repository at {:?}: {e}", repo_path);
                    // Skip creating a tab for failed repos
                }
            }
        }
        // Ensure at least one tab exists (even if all repos failed to open)
        if tabs.is_empty() {
            anyhow::bail!("No repositories could be opened");
        }

        let mut settings_dialog = SettingsDialog::new();
        settings_dialog.show_avatars = config.avatars_enabled;
        settings_dialog.scroll_speed = if config.fast_scroll { 2.0 } else { 1.0 };
        settings_dialog.row_scale = config.row_scale;
        settings_dialog.abbreviate_worktree_names = config.abbreviate_worktree_names;
        settings_dialog.time_spacing_strength = config.time_spacing_strength;
        settings_dialog.show_orphaned_commits = config.show_orphaned_commits;
        settings_dialog.ratchet_scroll = config.ratchet_scroll;
        let shortcut_bar_visible = config.shortcut_bar_visible;
        let ai_provider = ai::AiProvider::from_config(&config.ai_provider);
        let token_dialog = TokenDialog::new();

        // Migrate plaintext tokens from config to system keychain
        {
            let has_plaintext = config.github_token.is_some() || !config.gitlab_tokens.is_empty();
            if has_plaintext && token_store::is_available() {
                let (migrated, errors) = token_store::migrate_from_config(
                    config.github_token.as_deref(),
                    &config.gitlab_tokens,
                );
                if migrated > 0 {
                    // Clear plaintext token values but keep host keys as registry
                    config.github_token = None;
                    for value in config.gitlab_tokens.values_mut() {
                        *value = String::new();
                    }
                    let _ = config.save();
                }
                for err in errors {
                    eprintln!("Token migration warning: {err}");
                }
            }
        }

        let (dirty_check_tx, dirty_check_rx) = std::sync::mpsc::channel();
        Ok(Self {
            cli_args,
            config,
            tabs,
            next_tab_id,
            active_tab: 0,
            tab_bar,
            repo_dialog: RepoDialog::new(),
            clone_dialog: CloneDialog::new(),
            settings_dialog,
            token_dialog,
            confirm_dialog: ConfirmDialog::new(),
            error_dialog: ErrorDialog::new(),
            branch_name_dialog: BranchNameDialog::new(),
            remote_dialog: RemoteDialog::new(),
            merge_dialog: MergeDialog::new(),
            rebase_dialog: RebaseDialog::new(),
            pull_dialog: PullDialog::new(),
            push_dialog: PushDialog::new(),
            active_modal: None,
            interrupted_modal: None,
            pending_confirm_action: None,
            toast_manager: ToastManager::new(),
            tooltip: Tooltip::new(),
            state: None,
            divider_drag: None,
            sidebar_ratio: 0.14,
            graph_ratio: 0.55,
            staging_preview_ratio: 0.40,
            shortcut_bar_visible,
            current_cursor: CursorIcon::Default,
            status_dirty: true,
            last_status_refresh: Instant::now(),
            app_start: Instant::now(),
            ai_commit_receiver: None,
            ai_provider,
            proxy,
            last_frame_time: Instant::now(),
            last_completed_frame: Instant::now(),
            consecutive_draw_errors: 0,
            last_ref_check: Instant::now(),
            clone_receiver: None,
            open_receiver: None,
            diagnostic_before: None,
            text_rebuild_receiver: None,
            resize_debounce: None,
            dirty_check_tx,
            dirty_check_rx,
            dirty_checks_in_flight: 0,
        })
    }

    pub(crate) fn init_state(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        // Create window
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Whisper Git")
                        .with_inner_size(winit::dpi::LogicalSize::new(1600, 900)),
                )
                .context("Failed to create window")?,
        );

        // Enable IME so we receive Ime::Commit events for text input
        window.set_ime_allowed(true);

        // Create Vulkan context (needs surface for device selection)
        let library = vulkano::VulkanLibrary::new().context("No Vulkan library")?;
        let required_extensions = vulkano::swapchain::Surface::required_extensions(event_loop)
            .context("Failed to get extensions")?;
        let instance = vulkano::instance::Instance::new(
            library,
            vulkano::instance::InstanceCreateInfo {
                enabled_extensions: required_extensions,
                ..Default::default()
            },
        )
        .context("Failed to create instance")?;

        let surface = vulkano::swapchain::Surface::from_window(instance.clone(), window.clone())
            .context("Failed to create surface")?;

        let ctx = VulkanContext::with_surface(instance, &surface)?;
        crash_log::set_vulkan_device(&ctx.device.physical_device().properties().device_name);

        // Create render pass with MSAA 4x.
        // The resolve target's final_layout is PresentSrc so the swapchain
        // image is in the spec-required layout for vkQueuePresentKHR.
        // Vulkano's then_swapchain_present() does not insert this transition
        // (upstream TODO in acquire_present.rs), so without this override the
        // image would be presented in TransferDstOptimal — which causes
        // corruption on some drivers, particularly multi-GPU X11 with
        // DRI3/PRIME.
        let image_format = SurfaceManager::choose_surface_format(&ctx, &surface)?;
        let render_pass = vulkano::single_pass_renderpass!(
            ctx.device.clone(),
            attachments: {
                msaa_color: {
                    format: image_format,
                    samples: 4,
                    load_op: Clear,
                    store_op: DontCare,
                },
                resolve_target: {
                    format: image_format,
                    samples: 1,
                    load_op: DontCare,
                    store_op: Store,
                    final_layout: ImageLayout::PresentSrc,
                },
            },
            pass: {
                color: [msaa_color],
                color_resolve: [resolve_target],
                depth_stencil: {},
            },
        )
        .context("Failed to create render pass")?;

        // Create surface manager (reuse the surface from device selection)
        let surface_mgr =
            SurfaceManager::new(&ctx, &surface, window.inner_size(), render_pass.clone())?;

        // Create text renderer
        let mut upload_builder = AutoCommandBufferBuilder::primary(
            ctx.command_buffer_allocator.clone(),
            ctx.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .context("Failed to create upload command buffer")?;

        // Build font atlas close to the active window scale so low-DPI displays
        // don't aggressively minify oversized glyph atlases.
        // CLI --scale still overrides this for deterministic screenshots.
        let window_scale = window.scale_factor();
        let atlas_build_scale = self.cli_args.screenshot_scale.unwrap_or(window_scale);
        let mut text_renderer = TextRenderer::new(
            ctx.memory_allocator.clone(),
            render_pass.clone(),
            &mut upload_builder,
            atlas_build_scale,
        )
        .context("Failed to create text renderer")?;

        let mut bold_text_renderer = TextRenderer::new_bold(
            ctx.memory_allocator.clone(),
            render_pass.clone(),
            &mut upload_builder,
            atlas_build_scale,
        )
        .context("Failed to create bold text renderer")?;
        text_renderer.set_render_scale(window_scale);
        bold_text_renderer.set_render_scale(window_scale);

        if std::env::var_os("WHISPER_TEXT_DIAG").is_some() {
            let max_monitor_scale = window
                .available_monitors()
                .map(|m| m.scale_factor())
                .fold(window_scale, f64::max);
            eprintln!(
                "text_diag init: window_scale={window_scale:.2} atlas_build_scale={atlas_build_scale:.2} max_monitor_scale={max_monitor_scale:.2}"
            );
        }

        let spline_renderer =
            SplineRenderer::new(ctx.memory_allocator.clone(), render_pass.clone())
                .context("Failed to create spline renderer")?;

        let avatar_renderer =
            AvatarRenderer::new(ctx.memory_allocator.clone(), render_pass.clone())
                .context("Failed to create avatar renderer")?;

        let mut icon_renderer =
            IconRenderer::new(ctx.memory_allocator.clone(), render_pass.clone())
                .context("Failed to create icon renderer")?;
        crate::ui::icon::register_builtin_icons(&mut icon_renderer);

        // Submit font atlas upload
        let upload_buffer = upload_builder
            .build()
            .context("Failed to build upload buffer")?;
        let upload_future = sync::now(ctx.device.clone())
            .then_execute(ctx.queue.clone(), upload_buffer)
            .context("Failed to execute upload")?
            .then_signal_fence_and_flush()
            .map_err(Validated::unwrap)
            .context("Failed to flush upload")?;
        upload_future
            .wait(None)
            .context("Failed to wait for upload")?;

        let previous_frame_end = Some(sync::now(ctx.device.clone()).boxed());

        // Initialize all tab views
        let scale = window_scale as f32;
        let row_scale = self.settings_dialog.row_scale;
        let abbreviate_wt = self.settings_dialog.abbreviate_worktree_names;
        let time_strength = self.settings_dialog.time_spacing_strength;
        let fast_scroll = self.config.fast_scroll;
        for (repo_tab, view_state) in &mut self.tabs {
            view_state.commit_graph_view.row_scale = row_scale;
            view_state.commit_graph_view.abbreviate_worktree_names = abbreviate_wt;
            view_state.commit_graph_view.time_spacing_strength = time_strength;
            view_state.commit_graph_view.fast_scroll = fast_scroll;
            view_state.commit_graph_view.ratchet_scroll = self.config.ratchet_scroll;
            view_state.repo_state_receiver = Some(init_tab_view(
                repo_tab,
                view_state,
                &text_renderer,
                scale,
                self.config.show_orphaned_commits,
                &self.proxy,
            ));
        }

        self.state = Some(RenderState {
            window,
            ctx,
            surface: surface_mgr,
            text_renderer,
            bold_text_renderer,
            spline_renderer,
            avatar_renderer,
            avatar_cache: AvatarCache::new(self.proxy.clone()),
            icon_renderer,
            previous_frame_end,
            frame_count: 0,
            scale_factor: window_scale,
            input_state: InputState::new(),
        });

        // Set event loop proxy for file picker wake-up
        self.repo_dialog.set_proxy(self.proxy.clone());
        self.clone_dialog.set_proxy(self.proxy.clone());

        // Initial status refresh for active tab
        self.refresh_status();

        // Crash log housekeeping
        crash_log::prune_crash_logs(10);
        if let Some(path) = crash_log::has_crash_since_last_exit() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            self.toast_manager.push(
                format!("Whisper-Git crashed last session. Log: {}", name),
                ToastSeverity::Info,
            );
        }
        crash_log::breadcrumb("init_state complete".to_string());

        // Ensure first frame is drawn immediately (don't rely on platform Resized event)
        if let Some(state) = &self.state {
            state.window.request_redraw();
        }

        Ok(())
    }
}
