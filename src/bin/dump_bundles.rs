//! Dump aetna bundle artifacts (svg + tree + draw_ops + lint +
//! shader_manifest) for whisper-git's scenes. CPU-only: no GPU, no
//! window.
//!
//! Repo paths come from positional CLI args (or default to the current
//! working directory). Missing repos are tolerated quietly so the
//! same invocation works in any environment.

use std::path::{Path, PathBuf};

use aetna_core::{App, BuildCx, Rect, render_bundle, write_bundle};
use anyhow::{Context, Result};

use whisper_git::{
    WhisperApp,
    repo_tab::{RepoTab, RepoView},
    ui_app::{ActiveModal, ContextMenuState, ContextTarget},
};

fn main() -> Result<()> {
    let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("out");
    let viewport = Rect::new(0.0, 0.0, 1600.0, 900.0);

    let cli_paths: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    let paths: Vec<PathBuf> = if cli_paths.is_empty() {
        vec![std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))]
    } else {
        cli_paths
    };

    let opened: Vec<RepoTab> = paths
        .iter()
        .filter_map(|p| match RepoTab::open(p) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!(
                    "skipping {} (not a git repo or open failed: {e})",
                    p.display()
                );
                None
            }
        })
        .collect();

    let scenes = build_scenes(&opened);

    let mut total_findings = 0;
    for (name, app) in scenes {
        let theme = app.theme();
        let cx = BuildCx::new(&theme);
        let mut tree = app.build(&cx);
        let bundle = render_bundle(&mut tree, viewport, Some(env!("CARGO_PKG_NAME")));
        let written = write_bundle(&bundle, &out_dir, &name).context("write_bundle")?;
        for p in &written {
            println!("wrote {}", p.display());
        }
        if !bundle.lint.findings.is_empty() {
            eprintln!(
                "\nlint findings ({} in {name}):",
                bundle.lint.findings.len()
            );
            eprint!("{}", bundle.lint.text());
            total_findings += bundle.lint.findings.len();
        }
    }

    if total_findings > 0 {
        eprintln!("\n{total_findings} total lint findings");
    }
    Ok(())
}

fn build_scenes(opened: &[RepoTab]) -> Vec<(String, WhisperApp)> {
    let mut scenes: Vec<(String, WhisperApp)> = Vec::new();

    // Always render the empty state.
    scenes.push((
        "chrome_no_repo".to_string(),
        WhisperApp::with_tabs(Vec::new()),
    ));

    if let Some(first) = opened.first() {
        scenes.push((
            "sidebar_default".to_string(),
            WhisperApp::with_tabs(vec![reopen(first)]),
        ));
        scenes.push((
            "sidebar_local_collapsed".to_string(),
            WhisperApp::with_tabs(vec![{
                let mut t = reopen(first);
                t.sidebar
                    .collapsed
                    .insert(whisper_git::repo_tab::SidebarSection::Local);
                t
            }]),
        ));
        scenes.push(("sidebar_shortcuts_collapsed".to_string(), {
            let mut app = WhisperApp::with_tabs(vec![reopen(first)]);
            app.shortcut_bar_visible = false;
            app
        }));
        // Pre-select the first changed file so the diff view actually
        // renders content rather than the placeholder.
        let diff_target = first.active_view().and_then(|v| {
            v.status
                .unstaged
                .first()
                .or_else(|| v.status.untracked.first())
                .or_else(|| v.status.staged.first())
                .map(|f| f.path.clone())
        });
        if let Some(diff_target) = diff_target {
            scenes.push((
                "diff_view".to_string(),
                WhisperApp::with_tabs(vec![{
                    let mut t = reopen(first);
                    if let Some(view) = t.active_view_mut() {
                        view.selected_diff_file = Some(diff_target);
                    }
                    t
                }]),
            ));
        }
        scenes.push(("history_view".to_string(), {
            let mut t = reopen(first);
            t.view_mode = RepoView::History;
            // Pre-select the most recent commit so the bundle shows the
            // selected-row treatment (raised bg + bright ring) and the
            // commit details pane has content to render.
            let pick = t.commits.first().map(|c| c.id);
            t.select_commit(pick);
            WhisperApp::with_tabs(vec![t])
        }));
        if let Some(commit_oid) = first.commits.first().map(|c| c.id) {
            scenes.push(("history_context_menu".to_string(), {
                let mut t = reopen(first);
                t.view_mode = RepoView::History;
                t.select_commit(Some(commit_oid));
                let mut app = WhisperApp::with_tabs(vec![t]);
                app.context_menu = Some(ContextMenuState {
                    pos: (480.0, 200.0),
                    target: ContextTarget::Commit(commit_oid),
                });
                app
            }));
        }
        scenes.push(("modal_settings".to_string(), {
            let mut app = WhisperApp::with_tabs(vec![reopen(first)]);
            app.active_modal = Some(ActiveModal::Settings);
            app
        }));
        scenes.push(("modal_confirm_destructive".to_string(), {
            let mut app = WhisperApp::with_tabs(vec![reopen(first)]);
            app.active_modal = Some(ActiveModal::Confirm {
                title: "Delete branch".to_string(),
                body: "Delete local branch 'feature/old' permanently?".to_string(),
                ok_label: "Delete".to_string(),
                destructive: true,
                action: whisper_git::ui_app::ConfirmAction::CloseTab(0),
            });
            app
        }));
        scenes.push(("modal_error".to_string(), {
            let mut app = WhisperApp::with_tabs(vec![reopen(first)]);
            app.active_modal = Some(ActiveModal::Error {
                title: "Push failed".to_string(),
                body: "remote rejected the push: non-fast-forward updates were rejected"
                    .to_string(),
            });
            app
        }));
        scenes.push(("sidebar_context_menu".to_string(), {
            let mut app = WhisperApp::with_tabs(vec![reopen(first)]);
            app.context_menu = Some(ContextMenuState {
                pos: (90.0, 200.0),
                target: ContextTarget::LocalBranch("main".to_string()),
            });
            app
        }));
    }

    if opened.len() >= 2 {
        scenes.push((
            "sidebar_multi_tab".to_string(),
            WhisperApp::with_tabs(opened.iter().map(reopen).collect()),
        ));
    }

    scenes
}

/// `RepoTab` doesn't impl Clone (GitRepo wraps libgit2 handles), so we
/// re-open per scene from the underlying path.
fn reopen(t: &RepoTab) -> RepoTab {
    let path = t
        .repo
        .workdir()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    RepoTab::open(path).expect("reopen succeeded once already")
}
