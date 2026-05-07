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
    host::run("Whisper Git", viewport, app)?;
    crash_log::mark_clean_exit();
    Ok(())
}

fn apply_screenshot_state(app: &mut WhisperApp, state: Option<&str>) {
    use whisper_git::repo_tab::RepoView;
    use whisper_git::ui_app::{ActiveModal, ConfirmAction};

    let Some(state) = state else { return };
    match state {
        "history" => {
            if let Some(tab) = app.tabs.first_mut() {
                tab.view_mode = RepoView::History;
                tab.selected_commit = tab.commits.first().map(|c| c.id);
            }
        }
        "diff" => {
            // Pick the first changed file so the diff viewer has content
            // to render. Fall through silently when no repos are open.
            if let Some(tab) = app.tabs.first_mut() {
                let pick = tab
                    .status
                    .unstaged
                    .first()
                    .or_else(|| tab.status.untracked.first())
                    .or_else(|| tab.status.staged.first())
                    .map(|f| f.path.clone());
                if let Some(p) = pick {
                    tab.selected_diff_file = Some(p);
                }
            }
        }
        "settings" => {
            app.active_modal = Some(ActiveModal::Settings);
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
