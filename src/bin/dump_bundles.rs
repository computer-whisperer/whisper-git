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
    repo_tab::{RepoTab, TimedOp},
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

    // Welcome view — empty config (no recent repos).
    scenes.push((
        "welcome_empty".to_string(),
        WhisperApp::with_tabs(Vec::new()),
    ));
    // Welcome view with a populated recent-repos list. Synthetic paths
    // exercise the recent-row layout without depending on real repos.
    scenes.push(("welcome_recents".to_string(), {
        let mut app = WhisperApp::with_tabs(Vec::new());
        app.config.recent_repos = vec![
            "/home/example/Projects/whisper-git".to_string(),
            "/home/example/Projects/aetna".to_string(),
            "/home/example/work/dotfiles".to_string(),
        ];
        app
    }));

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
                        view.selected_diff_file = Some(diff_target.clone());
                    }
                    t
                }]),
            ));
            scenes.push(("diff_view_split".to_string(), {
                let mut app = WhisperApp::with_tabs(vec![{
                    let mut t = reopen(first);
                    if let Some(view) = t.active_view_mut() {
                        view.selected_diff_file = Some(diff_target);
                    }
                    t
                }]);
                app.config.diff_split = true;
                app
            }));
        }
        scenes.push(("history_view".to_string(), {
            let mut t = reopen(first);
            // Pre-select the first *real* commit (skip synthetic
            // worktree rows at the top of the graph) so the bundle
            // shows the right pane in commit-detail mode rather than
            // falling through to the default staging well.
            let pick = t.commits.iter().find(|c| !c.is_synthetic).map(|c| c.id);
            t.select_commit(pick);
            WhisperApp::with_tabs(vec![t])
        }));
        if let Some(commit_oid) = first
            .commits
            .iter()
            .find(|c| !c.is_synthetic)
            .map(|c| c.id)
        {
            scenes.push(("history_context_menu".to_string(), {
                let mut t = reopen(first);
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
        scenes.push(("modal_confirm_force_push".to_string(), {
            let mut app = WhisperApp::with_tabs(vec![reopen(first)]);
            app.active_modal = Some(ActiveModal::Confirm {
                title: "Push rejected".to_string(),
                body: "Push rejected: Remote has new commits. Pull first, or use Force Push.\n\n\
                       remote rejected the push: non-fast-forward updates were rejected\n\n\
                       Force push will overwrite remote history. Only do this if you're certain \
                       no one else has based work on main."
                    .to_string(),
                ok_label: "Force push".to_string(),
                destructive: true,
                action: whisper_git::ui_app::ConfirmAction::ForcePush {
                    remote: "origin".to_string(),
                    branch: "main".to_string(),
                },
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
        scenes.push(("modal_clone".to_string(), {
            use whisper_git::dialogs::CloneForm;
            let mut app = WhisperApp::with_tabs(vec![reopen(first)]);
            let mut form = CloneForm::default();
            form.url = "https://github.com/example/widget.git".to_string();
            form.dest = "/home/example/Projects/widget".to_string();
            app.active_modal = Some(ActiveModal::Clone(form));
            app
        }));
        scenes.push(("modal_token".to_string(), {
            use whisper_git::dialogs::TokenForm;
            let mut app = WhisperApp::with_tabs(vec![reopen(first)]);
            app.active_modal = Some(ActiveModal::Token(TokenForm::default()));
            app
        }));
        scenes.push(("modal_token_edit".to_string(), {
            use whisper_git::dialogs::TokenForm;
            let mut app = WhisperApp::with_tabs(vec![reopen(first)]);
            let mut form = TokenForm::default();
            form.editing_github = true;
            form.github_input = "ghp_demo123".to_string();
            app.active_modal = Some(ActiveModal::Token(form));
            app
        }));
        // Token modal with two registered GitLab hosts — one row in
        // edit mode, one in idle "configured" state — to exercise the
        // multi-host layout independently of the GitHub block.
        scenes.push(("modal_token_gitlab".to_string(), {
            use whisper_git::dialogs::TokenForm;
            let mut app = WhisperApp::with_tabs(vec![reopen(first)]);
            app.config.gitlab_hosts = vec![
                "gitlab.com".to_string(),
                "gitlab.company.example".to_string(),
            ];
            let mut form = TokenForm::default();
            form.gitlab_inputs
                .insert("gitlab.company.example".to_string(), "glpat_demo".to_string());
            app.active_modal = Some(ActiveModal::Token(form));
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
        // Header progress affordance — fresh fetch.
        scenes.push(("header_fetch_busy".to_string(), {
            let mut app = WhisperApp::with_tabs(vec![reopen(first)]);
            app.tabs[0].fetch_op = Some(synthetic_op("from origin", 4));
            app
        }));
        // Header progress affordance — fetch past the 60s stall threshold.
        scenes.push(("header_fetch_stalled".to_string(), {
            let mut app = WhisperApp::with_tabs(vec![reopen(first)]);
            app.tabs[0].fetch_op = Some(synthetic_op("from origin", 75));
            app
        }));
        // Two simultaneous ops — fresh push alongside a long-running cherry-pick.
        scenes.push(("header_push_and_mutation".to_string(), {
            let mut app = WhisperApp::with_tabs(vec![reopen(first)]);
            app.tabs[0].push_op = Some(synthetic_op("main \u{2192} origin", 8));
            app.tabs[0].mutation_op = Some(synthetic_op("cherry-pick abc1234", 90));
            app
        }));
        // CI surfaces — header-bar badges + per-commit dots in the
        // graph. Synthetic ProviderCiResults exercise both providers
        // and a mix of states so the bundle covers the full visual
        // vocabulary in one scene.
        scenes.push(("history_with_ci".to_string(), {
            let mut t = reopen(first);
            inject_synthetic_ci(&mut t);
            WhisperApp::with_tabs(vec![t])
        }));
        // Submodule surfaces — a synthetic submodule list on the active
        // worktree (drives the staging-well section) and synthetic
        // pinned entries on the selected commit's CommitDetail (drives
        // the commit-detail card). Real repos are unlikely to be
        // checked out with submodules in this env, so we synthesize
        // both here to cover the visuals.
        scenes.push(("staging_with_submodules".to_string(), {
            let mut t = reopen(first);
            inject_synthetic_submodules_active_view(&mut t);
            WhisperApp::with_tabs(vec![t])
        }));
        scenes.push(("commit_detail_with_submodules".to_string(), {
            let mut t = reopen(first);
            let pick = t.commits.iter().find(|c| !c.is_synthetic).map(|c| c.id);
            t.select_commit(pick);
            inject_synthetic_submodules_commit_detail(&mut t);
            WhisperApp::with_tabs(vec![t])
        }));
        // Drilled-in breadcrumb scene — pushes a synthetic submodule
        // RepoTab onto the nav stack. The "submodule" reuses the same
        // working repo (the test environment doesn't have a real
        // submodule lying around) but its repo_name is renamed so the
        // breadcrumb reads `<repo> › vendor/embassy` and the focused
        // view paints from the pushed entry.
        scenes.push(("drilled_into_submodule".to_string(), {
            let mut outer = reopen(first);
            let mut inner = reopen(first);
            inner.repo_name = "vendor/embassy".to_string();
            // Pin the parent's expected commit a few rows below HEAD so
            // the PINNED pill is visibly distinct from the HEAD/branch
            // pills on the top row. Picking the 4th real commit gives
            // the typical "the parent is N commits behind us" shape.
            inner.pinned_oid = inner
                .commits
                .iter()
                .filter(|c| !c.is_synthetic)
                .nth(3)
                .map(|c| c.id);
            outer.nav_stack.push(inner);
            WhisperApp::with_tabs(vec![outer])
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

/// Build a `TimedOp` for header-busy bundle scenes. The receiver is
/// connected to a leaked sender so it never disconnects while the build
/// renders; backdating `started` exercises the stall threshold.
fn synthetic_op(label: &str, age_secs: u64) -> TimedOp {
    let (tx, rx) = std::sync::mpsc::channel();
    // Bundle dumps build once and exit, but leak the sender so we don't
    // accidentally race the rx::Disconnected path if anyone polls.
    Box::leak(Box::new(tx));
    let mut op = TimedOp::new(rx, label);
    op.started = std::time::Instant::now() - std::time::Duration::from_secs(age_secs);
    op
}

/// Synthetic CI fixture: a GitHub failure on the head commit, a GitLab
/// success, and per-commit rollups across the first few real commits so
/// the graph rows show the dot strip. Exercises every state — Success,
/// Failure, Pending — for both surfaces in one bundle.
fn inject_synthetic_ci(tab: &mut RepoTab) {
    use std::collections::HashMap;
    use whisper_git::ci::{
        CiCheckStatus, CiCommitRollup, CiCounts, CiFetchResult, CiProvider, CiState, CiStatus,
        ProviderCiResult,
    };

    let real: Vec<git2::Oid> = tab
        .commits
        .iter()
        .filter(|c| !c.is_synthetic)
        .take(6)
        .map(|c| c.id)
        .collect();
    if real.is_empty() {
        return;
    }
    let head = real[0].to_string();

    let gh_runs = vec![
        CiCheckStatus {
            label: "build".into(),
            state: CiState::Failure,
            url: Some("https://example.com/runs/build".into()),
        },
        CiCheckStatus {
            label: "test".into(),
            state: CiState::Success,
            url: None,
        },
        CiCheckStatus {
            label: "lint".into(),
            state: CiState::Pending,
            url: None,
        },
    ];
    let gh_counts = CiCounts::from_states(gh_runs.iter().map(|c| c.state));
    let mut gh_per_commit: HashMap<String, CiCommitRollup> = HashMap::new();
    gh_per_commit.insert(
        head.clone(),
        CiCommitRollup {
            counts: gh_counts,
            checks: gh_runs.clone(),
        },
    );
    // Mix in success / pending across older commits so the dot strip
    // varies down the rows.
    for (i, oid) in real.iter().enumerate().skip(1) {
        let state = match i % 3 {
            0 => CiState::Success,
            1 => CiState::Pending,
            _ => CiState::Failure,
        };
        let counts = CiCounts::from_states(std::iter::once(state));
        gh_per_commit.insert(
            oid.to_string(),
            CiCommitRollup {
                counts,
                checks: vec![CiCheckStatus {
                    label: "build".into(),
                    state,
                    url: None,
                }],
            },
        );
    }
    let github = ProviderCiResult {
        provider: CiProvider::GitHub,
        status: CiStatus {
            state: CiState::Failure,
            summary: "1 failed, 1 pending, 1 passed".into(),
            url: Some("https://github.com/example/repo/actions".into()),
            counts: Some(gh_counts),
        },
        per_commit_rollups: gh_per_commit,
    };

    let mut gl_per_commit: HashMap<String, CiCommitRollup> = HashMap::new();
    gl_per_commit.insert(
        head.clone(),
        CiCommitRollup {
            counts: CiCounts {
                success: 1,
                failure: 0,
                pending: 0,
            },
            checks: vec![CiCheckStatus {
                label: "Pipeline #4242".into(),
                state: CiState::Success,
                url: Some("https://gitlab.example/pipeline/4242".into()),
            }],
        },
    );
    let gitlab = ProviderCiResult {
        provider: CiProvider::GitLab,
        status: CiStatus {
            state: CiState::Success,
            summary: "Pipeline passed".into(),
            url: Some("https://gitlab.example/pipelines".into()),
            counts: Some(CiCounts {
                success: 1,
                failure: 0,
                pending: 0,
            }),
        },
        per_commit_rollups: gl_per_commit,
    };

    tab.ci_results = vec![github, gitlab];
    let merged = CiFetchResult {
        providers: tab.ci_results.clone(),
    };
    tab.ci_per_commit = merged.per_commit_provider_rollups();
}

/// Synthetic submodules on the active worktree view — covers each
/// visible status (clean / staged-pointer / modified / drift) so the
/// staging-well section paints the full vocabulary.
fn inject_synthetic_submodules_active_view(tab: &mut RepoTab) {
    use whisper_git::git::SubmoduleInfo;
    let path = match tab.active_worktree.clone() {
        Some(p) => p,
        None => return,
    };
    let view = match tab.worktree_views.get_mut(&path) {
        Some(v) => v,
        None => return,
    };
    let head = git2::Oid::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
    let other = git2::Oid::from_str("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
    view.submodules = vec![
        SubmoduleInfo {
            name: "vendor/embassy".into(),
            path: "vendor/embassy".into(),
            branch: "main".into(),
            is_dirty: Some(false),
            head_oid: Some(head),
            index_oid: Some(head),
            workdir_oid: Some(head),
        },
        SubmoduleInfo {
            name: "vendor/nanoarrow".into(),
            path: "vendor/nanoarrow".into(),
            branch: "release/1.0".into(),
            is_dirty: Some(true),
            head_oid: Some(head),
            index_oid: Some(head),
            workdir_oid: Some(head),
        },
        SubmoduleInfo {
            name: "third_party/oggopus".into(),
            path: "third_party/oggopus".into(),
            branch: "main".into(),
            is_dirty: Some(false),
            head_oid: Some(head),
            index_oid: Some(other),
            workdir_oid: Some(other),
        },
        SubmoduleInfo {
            name: "third_party/trouble".into(),
            path: "third_party/trouble".into(),
            branch: "main".into(),
            is_dirty: Some(false),
            head_oid: Some(head),
            index_oid: Some(head),
            workdir_oid: Some(other),
        },
    ];
}

/// Synthetic CommitSubmoduleEntry list on the cached commit detail —
/// covers the changed (parent → pinned), new, and unchanged rows so
/// the commit-detail card paints the full vocabulary. Trims the
/// real commit's full_message down to the subject so the card lands
/// inside the bundle viewport (otherwise it sits below the fold and
/// only the interactive scroll exposes it).
fn inject_synthetic_submodules_commit_detail(tab: &mut RepoTab) {
    use whisper_git::git::CommitSubmoduleEntry;
    let detail = match tab.commit_detail.as_mut() {
        Some(d) => d,
        None => return,
    };
    detail.info.full_message = detail
        .info
        .full_message
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    let parent = git2::Oid::from_str("1111111111111111111111111111111111111111").unwrap();
    let pinned = git2::Oid::from_str("2222222222222222222222222222222222222222").unwrap();
    let unchanged = git2::Oid::from_str("3333333333333333333333333333333333333333").unwrap();
    let new_pin = git2::Oid::from_str("4444444444444444444444444444444444444444").unwrap();
    detail.submodule_entries = vec![
        CommitSubmoduleEntry {
            name: "vendor/embassy".into(),
            path: "vendor/embassy".into(),
            pinned_oid: pinned,
            changed: true,
            parent_oid: Some(parent),
        },
        CommitSubmoduleEntry {
            name: "vendor/new-arrival".into(),
            path: "vendor/new-arrival".into(),
            pinned_oid: new_pin,
            changed: true,
            parent_oid: None,
        },
        CommitSubmoduleEntry {
            name: "third_party/oggopus".into(),
            path: "third_party/oggopus".into(),
            pinned_oid: unchanged,
            changed: false,
            parent_oid: Some(unchanged),
        },
    ];
}
