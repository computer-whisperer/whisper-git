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
    /// Recognized but ignored in Phase 0 (no states wired yet).
    #[allow(dead_code)]
    screenshot_state: Option<String>,
    /// Recognized but ignored in Phase 0.
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
    let app = WhisperApp::new(args.repos.clone());

    if let Some(out_path) = args.screenshot.as_ref() {
        let (w, h) = args.screenshot_size.unwrap_or((DEFAULT_WIDTH, DEFAULT_HEIGHT));
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
