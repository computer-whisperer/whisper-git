//! whisper-git library crate.
//!
//! UI is built on the aetna toolkit; the renderer is `aetna-vulkano`
//! over our own `Arc<Device>` / `Arc<Queue>`. The git backend under
//! `git/` is preserved verbatim from the pre-port app.

pub mod avatar;
pub mod ci;
pub mod commit_details;
pub mod commit_graph;
pub mod config;
pub mod crash_log;
pub mod dialogs;
pub mod diff_view;
pub mod git;
pub mod git_async;
pub mod github;
pub mod gitlab;
pub mod host;
pub mod repo_tab;
pub mod screenshot_mode;
pub mod sidebar;
pub mod staging;
pub mod token_store;
pub mod ui_app;
pub mod watcher;
pub mod welcome;
pub mod widgets;

pub use ui_app::WhisperApp;
