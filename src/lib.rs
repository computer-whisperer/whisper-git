//! whisper-git library crate.
//!
//! Phase 0 surface: just the placeholder `WhisperApp` impl plus the
//! windowed and screenshot-mode hosts. The pre-port app modules live
//! on disk under `src/` but are intentionally not declared here while
//! the port is in flight.

pub mod crash_log;
pub mod git;
pub mod host;
pub mod repo_tab;
pub mod screenshot_mode;
pub mod sidebar;
pub mod staging;
pub mod ui_app;

pub use ui_app::WhisperApp;
