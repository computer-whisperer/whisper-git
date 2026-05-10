//! Whisper Git — Phase 0 entry.
//!
//! The pre-port modules (`ui/`, `views/`, `app_*.rs`, `rendering.rs`,
//! `messages/`, etc.) are still on disk for reference but no longer
//! compiled — they get re-enabled and ported across phases 2–7.

use std::path::PathBuf;

use aetna_core::Rect;
use anyhow::{Context, Result};

use whisper_git::{WhisperApp, crash_log, host, screenshot_mode};

#[derive(Default)]
struct CliArgs {
    screenshot: Option<PathBuf>,
    screenshot_size: Option<(u32, u32)>,
    screenshot_scale: Option<f64>,
    /// Optional state injection for screenshots. Recognized values:
    /// `diff` — auto-select the first changed file so the diff view
    /// renders content. Other states are added as they become useful.
    screenshot_state: Option<String>,
    /// Recognized but unused. Reserved for view selection in later phases.
    #[allow(dead_code)]
    view: Option<String>,
    repos: Vec<PathBuf>,
}

fn parse_args() -> CliArgs {
    let mut args = CliArgs::default();
    let mut iter = std::env::args().skip(1);

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--screenshot" => args.screenshot = iter.next().map(PathBuf::from),
            "--size" => {
                if let Some(size_str) = iter.next()
                    && let Some((w, h)) = size_str.split_once('x')
                    && let (Ok(width), Ok(height)) = (w.parse(), h.parse())
                {
                    args.screenshot_size = Some((width, height));
                }
            }
            "--scale" => {
                if let Some(s) = iter.next() {
                    args.screenshot_scale = s.parse().ok();
                }
            }
            "--screenshot-state" => args.screenshot_state = iter.next(),
            "--view" => args.view = iter.next(),
            "--repo" => {
                if let Some(p) = iter.next() {
                    args.repos.push(PathBuf::from(p));
                }
            }
            other if !other.starts_with('-') => args.repos.push(PathBuf::from(other)),
            _ => {}
        }
    }

    args
}

const DEFAULT_WIDTH: u32 = 1600;
const DEFAULT_HEIGHT: u32 = 900;

fn main() -> Result<()> {
    crash_log::init();
    crash_log::install_panic_hook();

    let args = parse_args();
    let mut app = WhisperApp::from_paths(args.repos.iter());

    if let Some(out_path) = args.screenshot.as_ref() {
        // Headless: no event loop, so the async-init path can't drain
        // results. Populate tabs synchronously before fixtures touch
        // them — `apply_screenshot_state` expects e.g. `tab.commits.first()`
        // to have data. The live (non-screenshot) path uses async init.
        for tab in &mut app.tabs {
            tab.refresh();
        }
        apply_screenshot_state(&mut app, args.screenshot_state.as_deref());
        let (w, h) = args
            .screenshot_size
            .unwrap_or((DEFAULT_WIDTH, DEFAULT_HEIGHT));
        let scale = args.screenshot_scale.unwrap_or(1.0) as f32;
        screenshot_mode::run(out_path, w, h, scale, app).context("screenshot mode")?;
        crash_log::mark_clean_exit();
        return Ok(());
    }

    let viewport = Rect::new(0.0, 0.0, DEFAULT_WIDTH as f32, DEFAULT_HEIGHT as f32);
    host::run("Whisper Git", viewport, app, |a, p| {
        a.proxy = Some(p);
    })?;
    crash_log::mark_clean_exit();
    Ok(())
}

fn apply_screenshot_state(app: &mut WhisperApp, state: Option<&str>) {
    use whisper_git::dialogs::{CloneForm, TokenForm};
    use whisper_git::ui_app::{ActiveModal, ConfirmAction};

    let Some(state) = state else { return };
    match state {
        "welcome" => {
            app.tabs.clear();
            app.config.recent_repos = vec![
                "/home/example/Projects/whisper-git".to_string(),
                "/home/example/Projects/aetna".to_string(),
                "/home/example/work/dotfiles".to_string(),
            ];
        }
        "history" => {
            if let Some(tab) = app.tabs.first_mut() {
                let pick = tab.commits.first().map(|c| c.id);
                tab.select_commit(pick);
                // Screenshot mode runs without a polling loop, so the
                // async diff-stats fetcher never gets a chance to land
                // its results. Block on the fetch synchronously here
                // so the +N/-M chip has data to render in the PNG.
                tab.fetch_diff_stats_sync();
            }
            prefetch_avatars_for_screenshot(app);
        }
        "history-search" => {
            if let Some(tab) = app.tabs.first_mut() {
                let pick = tab.commits.first().map(|c| c.id);
                tab.select_commit(pick);
                tab.fetch_diff_stats_sync();
                // Synthetic query that matches "graph:" prefix commits
                // — exercises the dim-non-matching-rows path so the
                // screenshot demonstrates the filter visually.
                tab.search_query = "graph".to_string();
                tab.history_search_open = true;
            }
            prefetch_avatars_for_screenshot(app);
        }
        "commit-menu" => {
            use whisper_git::ui_app::{ContextMenuState, ContextTarget};
            if let Some(tab) = app.tabs.first_mut() {
                if let Some(oid) = tab.commits.first().map(|c| c.id) {
                    tab.select_commit(Some(oid));
                    app.context_menu = Some(ContextMenuState {
                        pos: (480.0, 200.0),
                        target: ContextTarget::Commit(oid),
                    });
                }
            }
        }
        "diff" => {
            // Pick the first changed file so the diff viewer has content
            // to render. Fall through silently when no repos are open.
            if let Some(view) = app.tabs.first_mut().and_then(|t| t.active_view_mut()) {
                let pick = view
                    .status
                    .unstaged
                    .first()
                    .or_else(|| view.status.untracked.first())
                    .or_else(|| view.status.staged.first())
                    .map(|f| f.path.clone());
                if let Some(p) = pick {
                    view.selected_diff_file = Some(p);
                }
            }
        }
        "settings" => {
            app.active_modal = Some(ActiveModal::Settings);
        }
        "open-repo" => {
            // Match welcome/dump_bundles fixtures so the modal has a
            // visible recent list rather than the bare action row.
            app.config.recent_repos = vec![
                "/home/example/Projects/whisper-git".to_string(),
                "/home/example/Projects/aetna".to_string(),
                "/home/example/work/dotfiles".to_string(),
            ];
            app.active_modal = Some(ActiveModal::OpenRepo);
        }
        "confirm" => {
            app.active_modal = Some(ActiveModal::Confirm {
                title: "Delete branch".to_string(),
                body: "Delete local branch 'feature/old' permanently?".to_string(),
                ok_label: "Delete".to_string(),
                destructive: true,
                action: ConfirmAction::CloseTab(0),
            });
        }
        "error" => {
            app.active_modal = Some(ActiveModal::Error {
                title: "Push failed".to_string(),
                body: "remote rejected the push: non-fast-forward updates were rejected"
                    .to_string(),
            });
        }
        "clone" => {
            let mut form = CloneForm::default();
            form.url = "https://github.com/example/widget.git".to_string();
            form.dest = "/home/example/Projects/widget".to_string();
            app.active_modal = Some(ActiveModal::Clone(form));
        }
        "token" => {
            app.active_modal = Some(ActiveModal::Token(TokenForm::default()));
        }
        "token-edit" => {
            let mut form = TokenForm::default();
            form.editing_github = true;
            form.github_input = "ghp_demo123".to_string();
            app.active_modal = Some(ActiveModal::Token(form));
        }
        "context-menu" => {
            use whisper_git::ui_app::{ContextMenuState, ContextTarget};
            if let Some(tab) = app.tabs.first() {
                let target = tab
                    .local_branches()
                    .first()
                    .map(|b| ContextTarget::LocalBranch((*b).to_string()));
                if let Some(target) = target {
                    app.context_menu = Some(ContextMenuState {
                        pos: (90.0, 200.0),
                        target,
                    });
                }
            }
        }
        other => eprintln!("warning: unknown --screenshot-state '{other}'"),
    }
}

/// Synchronously fetch Gravatars for every author the screenshot
/// will render, so the PNG shows real avatars rather than the
/// identicon fallback. Skips when WHISPER_SKIP_AVATARS is set so
/// offline / sandboxed dev environments don't hang on the fetch.
fn prefetch_avatars_for_screenshot(app: &mut WhisperApp) {
    if std::env::var("WHISPER_SKIP_AVATARS").is_ok() {
        return;
    }
    let mut cache = whisper_git::avatar::AvatarCache::new_sync_only();
    let emails: Vec<String> = app
        .tabs
        .iter()
        .flat_map(|t| t.commits.iter())
        .map(|c| c.author_email.clone())
        .collect();
    for email in &emails {
        cache.prefetch_sync(email);
    }
    app.avatar_cache = Some(cache);
}
