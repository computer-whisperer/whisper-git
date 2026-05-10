//! Phase 3 App impl: chrome + branch sidebar wired to a real GitRepo.
//!
//! Per-tab data lives in `RepoTab` (see `repo_tab.rs`); the sidebar
//! composer lives in `sidebar.rs`. Staging / diff / graph still
//! placeholders in the main area.

use std::path::Path;

use aetna_core::{
    App, BuildCx, El, IconName, KeyChord, KeyModifiers, Selection, Theme, UiEvent,
    UiEventKind, UiKey,
    prelude::*,
    toast::ToastSpec,
    widgets::{
        resize_handle::{self, ResizeDrag, Side, resize_handle},
        text_area, text_input,
    },
};

const KM_CTRL: KeyModifiers = KeyModifiers {
    shift: false,
    ctrl: true,
    alt: false,
    logo: false,
};

/// Resize-handle clamp range for the right pane. Loose enough that the
/// commit-details / staging well can shrink to a usable narrow column
/// or expand into a roomy editor — tighter than the sidebar (which has
/// less content variance) but wider than the input minimums imply.
const RIGHT_PANE_MIN: f32 = 280.0;
const RIGHT_PANE_MAX: f32 = 720.0;

use crate::commit_details;
use crate::commit_graph;
use crate::config::Config;
use crate::dialogs;
use crate::diff_view;
use crate::git::{RemoteOpResult, classify_git_error};
use crate::dialogs::{
    BranchForm, CloneForm, PullForm, PushForm, TagForm, TokenForm, WorktreeForm,
};
use crate::repo_tab::{RepoTab, SidebarSection, TimedOp};
use crate::token_store;
use crate::sidebar;
use crate::staging;
use crate::welcome;

/// Resolve `(outer_idx, depth)` to a `&mut RepoTab`. `depth = None`
/// returns the outermost; `depth = Some(d)` indexes into the nav_stack
/// of that outermost. Returns `None` if either index is out of range.
fn resolve_tab_mut(
    tabs: &mut [RepoTab],
    idx: usize,
    depth: Option<usize>,
) -> Option<&mut RepoTab> {
    let outer = tabs.get_mut(idx)?;
    match depth {
        None => Some(outer),
        Some(d) => outer.nav_stack.get_mut(d),
    }
}

/// One level's CI poll: checks the dynamic interval against
/// `last_ci_fetch`, kicks off a refresh when due. Pulled to a free fn
/// so `poll_ci_refresh` can apply it to the outermost tab and to every
/// drilled-in level uniformly.
fn poll_ci_refresh_for(
    tab: &mut RepoTab,
    config: &mut Config,
    proxy: &winit::event_loop::EventLoopProxy<()>,
    now: std::time::Instant,
) {
    if !tab.ci_receivers.is_empty() {
        return;
    }
    let any_pending = tab
        .ci_results
        .iter()
        .any(|r| r.status.state == crate::ci::CiState::Pending);
    let recently_pushed = tab
        .last_push_time
        .is_some_and(|t| now.duration_since(t).as_secs() < 300);
    let interval_secs = if any_pending || recently_pushed {
        15
    } else {
        300
    };
    let due = match tab.last_ci_fetch {
        None => true,
        Some(last) => now.duration_since(last).as_secs() >= interval_secs,
    };
    if due {
        tab.trigger_ci_fetch(config, proxy.clone());
    }
}

/// Discriminator for the four per-tab async slots. Carries the
/// human-readable verbs used in toasts / error modal titles.
#[derive(Clone, Copy)]
enum AsyncKind {
    Fetch,
    Pull,
    Push,
    Mutation,
}

impl AsyncKind {
    fn name(self) -> &'static str {
        match self {
            Self::Fetch => "Fetch",
            Self::Pull => "Pull",
            Self::Push => "Push",
            Self::Mutation => "Operation",
        }
    }

    fn past(self) -> &'static str {
        match self {
            Self::Fetch => "Fetched",
            Self::Pull => "Pulled",
            Self::Push => "Pushed",
            Self::Mutation => "Done",
        }
    }
}

/// Pending action that gates a Confirm modal. Carried through `on_event`
/// from the originating action to the OK button.
#[derive(Clone, Debug)]
pub enum ConfirmAction {
    CloseTab(usize),
    DeleteBranch(String),
    DeleteTag(String),
    DropStash(usize),
    /// `git reset --hard <oid>` from a commit-row context menu.
    ResetHard(git2::Oid),
    /// `git push --force-with-lease` after a regular push was rejected
    /// non-fast-forward. Carries the same remote/branch the original
    /// push targeted so the retry hits the same ref.
    ForcePush {
        remote: String,
        branch: String,
    },
    /// Stage the parent's submodule pointer at `sm_path` to the new
    /// commit, then pop back to the parent view. Triggered by the
    /// post-commit coordination dialog when the user commits in a
    /// submodule and the new HEAD diverges from the pinned OID.
    UpdateSubmodulePin {
        sm_path: String,
    },
}

/// Per-section right-click target. Carries the exact identity needed to
/// dispatch any of that section's menu actions.
#[derive(Clone, Debug)]
pub enum ContextTarget {
    LocalBranch(String),
    RemoteBranch {
        remote: String,
        branch: String,
    },
    Tag(String),
    Stash(usize),
    /// A row in the commit history view.
    Commit(git2::Oid),
}

#[derive(Clone, Debug)]
pub struct ContextMenuState {
    pub pos: (f32, f32),
    pub target: ContextTarget,
}

const SIDEBAR_CTX_KEY: &str = "sidebar_ctx";

#[derive(Clone, Debug)]
pub enum ActiveModal {
    Settings,
    /// Open-repo picker: shown when the user clicks the tab-bar `+`.
    /// Surfaces the same "Open Local… / Clone Remote… / Recent" choices
    /// the welcome view offers, so a user with tabs already open can
    /// reach for a recent path without going through a file dialog.
    /// Carries no form state — the recent list comes straight from
    /// `Config::recent_repos` at render time.
    OpenRepo,
    Confirm {
        title: String,
        body: String,
        ok_label: String,
        destructive: bool,
        action: ConfirmAction,
    },
    Error {
        title: String,
        body: String,
    },
    /// Clone-a-remote dialog. Carries the live form state so the
    /// inputs persist while the user is editing.
    Clone(CloneForm),
    /// Manage Tokens dialog. Holds the inline-edit buffer for the
    /// GitHub token field; persistence goes through `token_store`.
    Token(TokenForm),
    /// Create-branch dialog. Carries the live form state (name +
    /// checkout toggle) plus the OID this branch will be created at —
    /// either the focused tab's selected_commit or its HEAD.
    Branch {
        form: BranchForm,
        target: git2::Oid,
    },
    /// Create-tag dialog. Mirrors `Branch` but tags are lightweight,
    /// so the form has no checkout toggle and HEAD doesn't move.
    Tag {
        form: TagForm,
        target: git2::Oid,
    },
    /// Pull-with-options picker. Lets the user pick a non-tracking
    /// source and toggle `--rebase`. Reached via the caret next to
    /// the header Pull button — the bare Pull button keeps its
    /// default-tracking-branch behavior.
    PullPicker {
        form: PullForm,
        sources: Vec<String>,
    },
    /// Push-with-options picker. Lets the user choose a remote, an
    /// override branch, and any of `--force-with-lease`,
    /// `--set-upstream`, `--tags`. Reached via the caret next to the
    /// header Push button.
    PushPicker {
        form: PushForm,
        remotes: Vec<String>,
    },
    /// Create-worktree dialog. Reached via the `+` icon on the
    /// trailing edge of the worktree pill bar above the staging well.
    Worktree {
        form: WorktreeForm,
    },
}


pub struct WhisperApp {
    pub tabs: Vec<RepoTab>,
    pub active_tab: usize,
    pub shortcut_bar_visible: bool,
    pub toasts: Vec<ToastSpec>,
    /// Global text selection. Aetna's `text_input` / `text_area`
    /// `apply_event` helpers fold per-input selection state through
    /// this single value (see `aetna_core::Selection::within`).
    pub selection: Selection,
    /// Persistent user settings. Loaded at startup, saved on each
    /// successful settings change. `Default` for fixture / dump scenes.
    pub config: Config,
    /// Currently-open modal, if any. Esc / scrim click / OK / Cancel
    /// all clear this back to None.
    pub active_modal: Option<ActiveModal>,
    /// Open context menu (one at a time). Outside-click dismisses.
    pub context_menu: Option<ContextMenuState>,
    /// Wakes the event loop when an async git op completes. Set by the
    /// host immediately after the loop is built (see [`host::run`]).
    /// `None` for headless use (`with_tabs` / dump_bundles); attempting
    /// to start an op without a proxy emits an error toast.
    pub proxy: Option<winit::event_loop::EventLoopProxy<()>>,
    /// In-flight `git clone`. App-scoped (not per-tab) since the new
    /// repo doesn't have a tab yet — on success we open it as one.
    pub clone_op: Option<CloneOp>,
    /// Left-sidebar pixel width. Initialised from `Config::sidebar_w`,
    /// re-saved when the user releases a drag of the left handle.
    pub sidebar_w: f32,
    /// Right-pane pixel width. Drives both the staging well (Working
    /// view) and the commit details pane (History view) so the user's
    /// choice carries between view modes.
    pub right_pane_w: f32,
    /// Drag-anchor state for the left and right resize handles.
    /// Ephemeral — not persisted, just rebuilt across PointerDown /
    /// Drag / PointerUp.
    pub sidebar_drag: ResizeDrag,
    pub right_drag: ResizeDrag,
    /// Gravatar avatar cache. Lazy — created the first time the
    /// polling loop runs with a `proxy` available, so headless apps
    /// (dump_bundles, screenshot mode) don't hold a worker channel
    /// they'll never feed.
    pub avatar_cache: Option<crate::avatar::AvatarCache>,
    /// Global channel for per-entity dirty-check results. Each spawned
    /// dirty-check worker (one per submodule, one per worktree) sends
    /// its result here; the polling loop drains and routes back to the
    /// originating `RepoTab` by `tab_id`. Keeping the channel global
    /// rather than per-tab keeps the drain to a single try_recv loop;
    /// stale results from closed tabs match no live tab and drop
    /// silently.
    pub dirty_check_tx: std::sync::mpsc::Sender<crate::git_async::DirtyCheckResult>,
    pub dirty_check_rx: std::sync::mpsc::Receiver<crate::git_async::DirtyCheckResult>,
    /// Total per-entity dirty checks currently running across all tabs.
    /// Decremented when their results land (regardless of which tab they
    /// targeted). Used to gate "kick off another fanout right now" so
    /// the system doesn't stack identical workers when the watcher
    /// fires repeatedly during a long-running scan.
    pub dirty_checks_in_flight: usize,
    /// `true` when the active tab needs a fresh status query (working-
    /// dir staleness from a watcher event, or the 30 s safety net
    /// expired). Drained by the next polling pass — single bit so
    /// repeated marks coalesce into one spawn.
    pub status_dirty: bool,
    /// Wall time of the last successful status refresh on the active
    /// tab. Drives the 30 s safety-net timer that flips `status_dirty`
    /// on if no watcher event has arrived in that window.
    pub last_status_refresh: std::time::Instant,
    /// Wall time of the last `ref_fingerprint` check. Compared every
    /// 5 s against the active tab's cached fingerprint; a divergence
    /// triggers `repo.reopen()` + a full state refresh — belt-and-
    /// braces against missed watcher events.
    pub last_ref_check: std::time::Instant,
}

/// In-flight clone tracker. Carries the receiver, the started time
/// (currently informational), and the destination path for the
/// success-path tab open.
pub struct CloneOp {
    pub rx: std::sync::mpsc::Receiver<Result<std::path::PathBuf, String>>,
    pub started: std::time::Instant,
    pub dest_label: String,
}

impl WhisperApp {
    /// Construct from CLI repo paths. Failed opens log to stderr and
    /// produce no tab.
    pub fn from_paths<I, P>(paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut tabs = Vec::new();
        for p in paths {
            let path = p.as_ref();
            match RepoTab::open(path) {
                Ok(tab) => tabs.push(tab),
                Err(e) => eprintln!(
                    "Warning: could not open repository at {}: {e}",
                    path.display()
                ),
            }
        }
        let config = Config::load();
        let sidebar_w = config.sidebar_w;
        let right_pane_w = config.right_pane_w;
        let (dirty_check_tx, dirty_check_rx) = std::sync::mpsc::channel();
        let now = std::time::Instant::now();
        Self {
            tabs,
            active_tab: 0,
            shortcut_bar_visible: config.shortcut_bar_visible,
            toasts: Vec::new(),
            selection: Selection::default(),
            config,
            active_modal: None,
            context_menu: None,
            proxy: None,
            clone_op: None,
            sidebar_w,
            right_pane_w,
            sidebar_drag: ResizeDrag::default(),
            right_drag: ResizeDrag::default(),
            avatar_cache: None,
            dirty_check_tx,
            dirty_check_rx,
            dirty_checks_in_flight: 0,
            status_dirty: false,
            last_status_refresh: now,
            last_ref_check: now,
        }
    }

    /// Construct with already-built tabs. Used by `dump_bundles` which
    /// fabricates synthetic repos. Config is `Default::default()` so
    /// dumped scenes are hermetic across developer machines.
    pub fn with_tabs(tabs: Vec<RepoTab>) -> Self {
        let config = Config::default();
        let sidebar_w = config.sidebar_w;
        let right_pane_w = config.right_pane_w;
        let (dirty_check_tx, dirty_check_rx) = std::sync::mpsc::channel();
        let now = std::time::Instant::now();
        Self {
            tabs,
            active_tab: 0,
            shortcut_bar_visible: true,
            toasts: Vec::new(),
            selection: Selection::default(),
            config,
            active_modal: None,
            context_menu: None,
            proxy: None,
            clone_op: None,
            sidebar_w,
            right_pane_w,
            sidebar_drag: ResizeDrag::default(),
            right_drag: ResizeDrag::default(),
            avatar_cache: None,
            dirty_check_tx,
            dirty_check_rx,
            dirty_checks_in_flight: 0,
            status_dirty: false,
            last_status_refresh: now,
            last_ref_check: now,
        }
    }

    /// Outermost RepoTab the user has opened (the one driving the
    /// editor-tabs strip + tab close). Use [`Self::active_focus`] for
    /// almost everything else — when the user has drilled into a
    /// submodule, `active()` still points at the parent while
    /// `active_focus()` points at the submodule view they're looking at.
    fn active(&self) -> Option<&RepoTab> {
        self.tabs.get(self.active_tab)
    }

    fn active_mut(&mut self) -> Option<&mut RepoTab> {
        self.tabs.get_mut(self.active_tab)
    }

    /// Currently focused view within the active tab — the deepest
    /// drilled-in submodule, or the tab itself at root. The renderer,
    /// most ops, and CI all consult this so submodule focus is
    /// transparent to widget code.
    fn active_focus(&self) -> Option<&RepoTab> {
        self.active().map(|t| t.active_view_tab())
    }

    fn active_focus_mut(&mut self) -> Option<&mut RepoTab> {
        self.active_mut().map(|t| t.active_view_tab_mut())
    }
}

impl App for WhisperApp {
    fn before_build(&mut self) {
        self.trigger_initial_state_refreshes();
        self.poll_async_ops();
    }

    fn build(&self, _cx: &BuildCx) -> El {
        let mut chrome: Vec<El> = Vec::with_capacity(3);
        if !self.tabs.is_empty() {
            chrome.push(tab_bar(self));
        }
        if let Some(outer) = self.active()
            && outer.nav_depth() > 0
        {
            chrome.push(breadcrumb_bar(&outer.nav_chain_names()));
            if let Some(strip) = parent_context_strip(outer) {
                chrome.push(strip);
            }
        }
        chrome.push(header_bar(self.active_focus(), self.clone_op.as_ref()));
        if self.shortcut_bar_visible {
            chrome.push(shortcut_bar());
        }
        let chrome_el = column(chrome);

        // Body composes against the *focused* tab — the deepest
        // drilled-in submodule when the user has navigated in,
        // otherwise the outermost tab. Widget code below this point
        // doesn't need to know about drill-down.
        let body = match self.active_focus() {
            Some(tab) => {
                // Center pane: graph by default; the diff temporarily
                // takes over when the user picks a file (in the staging
                // well or in a selected commit's file list). The graph
                // is the home base — Escape unwinds back to it.
                let center = match tab.active_view() {
                    Some(view) if view.selected_diff_file.is_some() => {
                        let mode = if self.config.diff_split {
                            crate::widgets::diff::DiffMode::Split
                        } else {
                            crate::widgets::diff::DiffMode::Unified
                        };
                        diff_view::diff_view(tab, mode)
                    }
                    _ => {
                        // Snapshot loaded Gravatars for the rows
                        // we're about to render. Cheap clone (Image
                        // is Arc-backed); the closure takes ownership
                        // and looks up by email.
                        let avatars = self
                            .avatar_cache
                            .as_ref()
                            .map(|c| {
                                tab.commits
                                    .iter()
                                    .filter_map(|cm| {
                                        c.get(&cm.author_email)
                                            .map(|img| (cm.author_email.clone(), img))
                                    })
                                    .collect::<std::collections::HashMap<_, _>>()
                            })
                            .unwrap_or_default();
                        commit_graph::history_view(tab, &self.selection, avatars)
                    }
                };

                // Right pane: worktree pill bar pinned at the top
                // (always-on handle for one-or-more worktrees + at-a-glance
                // dirty count), then either the commit detail (when a
                // commit is selected) or the staging well (default).
                let right_upper = if tab.selected_commit.is_some() {
                    commit_details::commit_details_pane(tab)
                } else if let Some(view) = tab.active_view() {
                    staging::staging_well(view, &self.selection, tab.ai_op.is_some())
                } else {
                    no_worktree_placeholder()
                };
                let mut right_children: Vec<El> = Vec::with_capacity(2);
                if let Some(pills) = staging::worktree_selector(tab) {
                    right_children.push(pills);
                }
                right_children.push(right_upper);
                let right = column(right_children).height(Size::Fill(1.0));

                // Resizable layout: sidebar | resize_handle | center
                // | resize_handle | right. The handle widgets live as
                // siblings inside the row and route drag events to the
                // app via their keys; aetna's `apply_event_fixed` folds
                // the drag delta back into the size value.
                let children: Vec<El> = vec![
                    sidebar::sidebar(tab).width(Size::Fixed(self.sidebar_w)),
                    resize_handle(Axis::Row).key("sidebar:resize"),
                    center,
                    resize_handle(Axis::Row).key("right:resize"),
                    right.width(Size::Fixed(self.right_pane_w)),
                ];
                let main_row = row(children).height(Size::Fill(1.0));
                // Sibling-submodule strip below the main split, only
                // when drilled in *and* the immediate parent has more
                // than one submodule (a strip with one entry is just
                // the current view — pure noise).
                if let Some(siblings_strip) = self
                    .active()
                    .and_then(|outer| sibling_submodule_strip(outer, tab))
                {
                    column([main_row, siblings_strip]).height(Size::Fill(1.0))
                } else {
                    main_row
                }
            }
            None => welcome::welcome_view(&self.config.recent_repos),
        };

        let main = column([chrome_el, body]);
        let modal_layer = self.active_modal.as_ref().map(|m| match m {
            ActiveModal::Settings => {
                dialogs::settings_modal(&self.config, self.shortcut_bar_visible)
            }
            ActiveModal::OpenRepo => dialogs::open_repo_modal(&self.config.recent_repos),
            ActiveModal::Confirm {
                title,
                body,
                ok_label,
                destructive,
                ..
            } => dialogs::confirm_modal(title, body, ok_label, *destructive),
            ActiveModal::Error { title, body } => dialogs::error_modal(title, body),
            ActiveModal::Clone(form) => {
                dialogs::clone_modal(form, &self.selection, self.clone_op.is_some())
            }
            ActiveModal::Token(form) => {
                let github_set = token_store::get_github_token().is_some();
                let gitlab_hosts: Vec<(String, bool)> = self
                    .config
                    .gitlab_hosts
                    .iter()
                    .map(|h| (h.clone(), token_store::get_gitlab_token(h).is_some()))
                    .collect();
                dialogs::token_modal(form, &self.selection, github_set, &gitlab_hosts)
            }
            ActiveModal::Branch { form, target } => {
                let target_short = target.to_string()[..7].to_string();
                dialogs::branch_modal(form, &self.selection, &target_short)
            }
            ActiveModal::Tag { form, target } => {
                let target_short = target.to_string()[..7].to_string();
                dialogs::tag_modal(form, &self.selection, &target_short)
            }
            ActiveModal::PullPicker { form, sources } => {
                dialogs::pull_modal(form, sources)
            }
            ActiveModal::PushPicker { form, remotes } => {
                dialogs::push_modal(form, &self.selection, remotes)
            }
            ActiveModal::Worktree { form } => {
                dialogs::worktree_modal(form, &self.selection)
            }
        });
        let menu_layer = self
            .context_menu
            .as_ref()
            .map(|cm| sidebar_context_menu(cm));
        overlays(main, [menu_layer, modal_layer])
    }

    fn on_event(&mut self, event: UiEvent) {
        // Escape unwinds the deepest active state, one step at a time:
        // (1) close any open modal, (2) clear the focused view's diff
        // (returns center to graph), (3) clear the focused view's
        // selected commit (returns right pane to staging well), (4) pop
        // one level of submodule drill-down. Aetna emits an Escape
        // event when the key is pressed and no widget consumes it; our
        // text inputs don't consume Escape, so it always reaches us.
        if matches!(event.kind, UiEventKind::Escape) {
            if self.active_modal.is_some() {
                self.active_modal = None;
                return;
            }
            // Search bar (Ctrl+F) closes before any other unwind step —
            // matches browser/editor convention. Clears the query so a
            // re-open starts fresh; the dim-non-matching-rows highlight
            // also goes away as a side effect.
            if let Some(focus) = self.active_focus_mut()
                && focus.history_search_open
            {
                focus.history_search_open = false;
                focus.search_query.clear();
                return;
            }
            if let Some(focus) = self.active_focus_mut() {
                let cleared_diff = focus
                    .active_view_mut()
                    .map(|v| {
                        let had = v.selected_diff_file.is_some();
                        v.selected_diff_file = None;
                        had
                    })
                    .unwrap_or(false);
                if cleared_diff {
                    return;
                }
                if focus.selected_commit.is_some() {
                    focus.select_commit(None);
                    return;
                }
            }
            // After per-focus-view unwinding, the next Escape pops one
            // level of submodule drill-down (so a single Escape climbs
            // the chain rather than escaping all the way out).
            if let Some(tab) = self.active_mut()
                && tab.exit_submodule()
            {
                return;
            }
            // Fall through: nothing to unwind, let the event propagate.
        }

        // Resize-handle drags. Each handle owns its anchor state on
        // `WhisperApp`; PointerUp persists the new width to disk so
        // the layout survives a relaunch. The `Side` parameter tells
        // aetna which sibling owns the value — `Start` for the
        // left-anchored sidebar, `End` for the right-anchored pane
        // (so drag-left grows it, drag-right shrinks it).
        if resize_handle::apply_event_fixed(
            &mut self.sidebar_w,
            &mut self.sidebar_drag,
            &event,
            "sidebar:resize",
            Axis::Row,
            Side::Start,
            tokens::SIDEBAR_WIDTH_MIN,
            tokens::SIDEBAR_WIDTH_MAX,
        ) {
            self.config.sidebar_w = self.sidebar_w;
        }
        if resize_handle::apply_event_fixed(
            &mut self.right_pane_w,
            &mut self.right_drag,
            &event,
            "right:resize",
            Axis::Row,
            Side::End,
            RIGHT_PANE_MIN,
            RIGHT_PANE_MAX,
        ) {
            self.config.right_pane_w = self.right_pane_w;
        }
        if matches!(event.kind, UiEventKind::PointerUp)
            && (event.route() == Some("sidebar:resize")
                || event.route() == Some("right:resize"))
        {
            // Save once on release rather than on every Drag tick — both
            // to avoid spamming the disk and so a settings.json read
            // partway through a drag sees a coherent value.
            let _ = self.config.save();
        }

        // Text-editing routes consume the event for the active worktree's
        // commit-message fields. Drafts are per-worktree — switching
        // worktrees swaps which subject/body buffer the inputs touch.
        let active_idx = self.active_tab;
        if let Some(tab) = self.tabs.get_mut(active_idx) {
            text_input::apply_event(
                &mut tab.search_query,
                &mut self.selection,
                commit_graph::SEARCH_INPUT_KEY,
                &event,
            );
        }
        if let Some(view) = self
            .tabs
            .get_mut(active_idx)
            .and_then(|t| t.active_view_mut())
        {
            text_input::apply_event(
                &mut view.commit_subject,
                &mut self.selection,
                "subject",
                &event,
            );
            text_area::apply_event(&mut view.commit_body, &mut self.selection, "body", &event);
        }

        // Modal text fields. Routed by key — only the active modal's
        // fields are present in the tree, so non-matching events are
        // ignored harmlessly.
        match &mut self.active_modal {
            Some(ActiveModal::Clone(form)) => {
                text_input::apply_event(&mut form.url, &mut self.selection, "clone:url", &event);
                text_input::apply_event(&mut form.dest, &mut self.selection, "clone:dest", &event);
            }
            Some(ActiveModal::Token(form)) => {
                text_input::apply_event(
                    &mut form.github_input,
                    &mut self.selection,
                    "token:github",
                    &event,
                );
                // Each in-edit GitLab host owns a routed text input
                // keyed `token:gitlab:input:<host>`. Iterate the form's
                // editing set so we don't pay a per-host fold for hosts
                // the user isn't currently editing.
                let host_keys: Vec<String> = form.gitlab_inputs.keys().cloned().collect();
                for host in host_keys {
                    let route = format!("token:gitlab:input:{host}");
                    if let Some(buf) = form.gitlab_inputs.get_mut(&host) {
                        text_input::apply_event(buf, &mut self.selection, &route, &event);
                    }
                }
            }
            Some(ActiveModal::Branch { form, .. }) => {
                text_input::apply_event(
                    &mut form.name,
                    &mut self.selection,
                    "branch:name",
                    &event,
                );
            }
            Some(ActiveModal::Tag { form, .. }) => {
                text_input::apply_event(
                    &mut form.name,
                    &mut self.selection,
                    "tag:name",
                    &event,
                );
            }
            Some(ActiveModal::PullPicker { form, .. }) => {
                aetna_core::widgets::radio::apply_event(
                    &mut form.source,
                    &event,
                    "pull:source",
                    |raw| Some(raw.to_string()),
                );
            }
            Some(ActiveModal::PushPicker { form, .. }) => {
                aetna_core::widgets::radio::apply_event(
                    &mut form.remote,
                    &event,
                    "push:remote",
                    |raw| Some(raw.to_string()),
                );
                text_input::apply_event(
                    &mut form.branch,
                    &mut self.selection,
                    "push:branch",
                    &event,
                );
            }
            Some(ActiveModal::Worktree { form }) => {
                text_input::apply_event(
                    &mut form.path,
                    &mut self.selection,
                    "worktree:path",
                    &event,
                );
                text_input::apply_event(
                    &mut form.source,
                    &mut self.selection,
                    "worktree:source",
                    &event,
                );
            }
            _ => {}
        }

        if matches!(event.kind, UiEventKind::SecondaryClick) && self.handle_secondary_click(&event)
        {
            return;
        }

        let route = event.route().map(str::to_string);
        match event.kind {
            UiEventKind::Click | UiEventKind::Activate | UiEventKind::Hotkey => {
                if let Some(key) = route.as_deref() {
                    self.handle_action(key);
                }
            }
            _ => {}
        }
    }

    fn selection(&self) -> Selection {
        self.selection.clone()
    }

    fn hotkeys(&self) -> Vec<(KeyChord, String)> {
        vec![
            (KeyChord::ctrl('o'), "open_repo".to_string()),
            (KeyChord::ctrl('w'), "close_tab".to_string()),
            (KeyChord::ctrl('/'), "toggle_shortcut_bar".to_string()),
            (KeyChord::ctrl('f'), "history:search_open".to_string()),
            (
                KeyChord::named(UiKey::Enter).with_modifiers(KM_CTRL),
                "commit".to_string(),
            ),
        ]
    }

    fn drain_toasts(&mut self) -> Vec<ToastSpec> {
        std::mem::take(&mut self.toasts)
    }

    fn theme(&self) -> Theme {
        Theme::radix_slate_blue_dark()
    }
}

impl WhisperApp {
    fn handle_action(&mut self, key: &str) {
        // Modal lifecycle keys come first — a few share prefixes with
        // app actions (e.g. "settings:" vs "settings"), and the modal
        // routes should always take precedence when one is open.
        if self.handle_modal_route(key) {
            return;
        }

        // editor_tabs: tabs:tab:{idx} (select), tabs:close:{idx} (close),
        // tabs:add (route to open-repo picker).
        if let Some(idx_str) = key.strip_prefix("tabs:close:") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                self.close_tab(idx);
            }
            return;
        }
        if let Some(idx_str) = key.strip_prefix("tabs:tab:") {
            if let Ok(idx) = idx_str.parse::<usize>()
                && idx < self.tabs.len()
            {
                self.active_tab = idx;
            }
            return;
        }
        if key == "tabs:add" {
            self.active_modal = Some(ActiveModal::OpenRepo);
            return;
        }

        // ci:open:{idx} — open the provider's URL in the system browser.
        // The index is into `active().ci_results`; values shift as
        // results refresh, but only between frames so the route the
        // user clicked is always valid by the time we read it.
        if let Some(idx_str) = key.strip_prefix("ci:open:") {
            if let Ok(idx) = idx_str.parse::<usize>()
                && let Some(url) = self
                    .active()
                    .and_then(|t| t.ci_results.get(idx))
                    .and_then(|r| r.status.url.clone())
            {
                let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
            }
            return;
        }

        // section:LOCAL etc.
        if let Some(section_key) = key.strip_prefix("section:") {
            if let Some(section) = parse_section(section_key)
                && let Some(tab) = self.active_focus_mut()
            {
                tab.sidebar.toggle(section);
            }
            return;
        }

        // Stage / unstage / diff-preview routes.
        if let Some(path) = key.strip_prefix("stage_file:") {
            let path = path.to_string();
            self.run_op("Stage", move |t| {
                t.active_repo().stage_file(&path)
            });
            return;
        }
        if let Some(path) = key.strip_prefix("unstage_file:") {
            let path = path.to_string();
            self.run_op("Unstage", move |t| {
                t.active_repo().unstage_file(&path)
            });
            return;
        }
        // diff:mode_toggle — flip between unified and split. Persist
        // the new preference so the user's choice survives a relaunch.
        if key == diff_view::DIFF_MODE_TOGGLE_KEY {
            self.config.diff_split = !self.config.diff_split;
            let _ = self.config.save();
            return;
        }
        if let Some(path) = key.strip_prefix("diff:") {
            if let Some(view) = self
                .active_focus_mut()
                .and_then(|t| t.active_view_mut())
            {
                view.selected_diff_file = Some(path.to_string());
            }
            return;
        }
        // wt_select:tab:{path} — switch the active worktree and jump
        // to the "about this worktree" state: drop any selected commit
        // and diff so the right pane shows the staging well and the
        // center returns to the graph. The path is the tab_trigger
        // value, routed verbatim (worktree names aren't always unique
        // across nested checkouts).
        if let Some(path) = key.strip_prefix("wt_select:tab:") {
            if let Some(tab) = self.active_focus_mut() {
                tab.select_worktree(std::path::PathBuf::from(path));
                tab.select_commit(None);
                if let Some(view) = tab.active_view_mut() {
                    view.selected_diff_file = None;
                }
            }
            return;
        }
        // commit:{idx} — selects a commit in the graph. The previous
        // diff_file (if any) is cleared because it might not exist in
        // the new commit context, and Escape's role is to unwind one
        // step at a time — clearing it here keeps the right-pane
        // upper swap from happening with stale center-pane state.
        if let Some(idx_str) = key.strip_prefix("commit:") {
            if let Ok(idx) = idx_str.parse::<usize>()
                && let Some(tab) = self.active_focus_mut()
            {
                let oid = tab.commits.get(idx).map(|c| c.id);
                tab.select_commit(oid);
                if let Some(view) = tab.active_view_mut() {
                    view.selected_diff_file = None;
                }
            }
            return;
        }
        // commit_file:{path} — clicking a file row in the commit
        // detail's files list. Pushes the diff into the center pane;
        // diff_view picks `tab.selected_commit` as the source.
        if let Some(path) = key.strip_prefix("commit_file:") {
            if let Some(view) = self
                .active_focus_mut()
                .and_then(|t| t.active_view_mut())
            {
                view.selected_diff_file = Some(path.to_string());
            }
            return;
        }
        if let Some(rest) = key.strip_prefix("stage_hunk:") {
            if let Some((idx_str, path)) = rest.split_once(':')
                && let Ok(idx) = idx_str.parse::<usize>()
            {
                let path = path.to_string();
                self.run_op("Stage hunk", move |t| {
                    t.active_repo().stage_hunk(&path, idx)
                });
            }
            return;
        }
        if let Some(rest) = key.strip_prefix("unstage_hunk:") {
            if let Some((idx_str, path)) = rest.split_once(':')
                && let Ok(idx) = idx_str.parse::<usize>()
            {
                let path = path.to_string();
                self.run_op("Unstage hunk", move |t| {
                    t.active_repo().unstage_hunk(&path, idx)
                });
            }
            return;
        }

        // Worktree pill click — promote that worktree to active.
        // Source: clean-worktree pills + the WT: pill on synthetic
        // worktree rows in the commit graph (the sidebar no longer
        // has a Worktrees section; the pill bar at the top of the
        // staging well is the primary affordance).
        if let Some(name) = key.strip_prefix("worktree:") {
            if let Some(tab) = self.active_focus_mut() {
                let path = tab
                    .worktrees
                    .iter()
                    .find(|w| w.name == name)
                    .map(|w| std::path::PathBuf::from(&w.path));
                if let Some(p) = path {
                    tab.select_worktree(p);
                }
            }
            return;
        }
        // welcome:recent:{idx} — open the persisted recent path at idx.
        if let Some(idx_str) = key.strip_prefix("welcome:recent:") {
            if let Ok(idx) = idx_str.parse::<usize>()
                && let Some(path) = self.config.recent_repos.get(idx).cloned()
            {
                self.open_repo_path(std::path::PathBuf::from(path));
            }
            return;
        }

        // Sidebar branch/remote/tag click — jump-to-commit. Resolve
        // the ref to its OID against the focused tab's metadata, then
        // call select_commit so the row highlights and the right pane
        // opens commit details. Aetna's virtual_list doesn't yet have a
        // scroll-to-index API, so off-viewport selections still need
        // the user to scroll — but the right pane shows the commit so
        // the click is never silent.
        if let Some(name) = key.strip_prefix("branch:") {
            let oid = self
                .active_focus()
                .and_then(|t| {
                    t.branch_tips
                        .iter()
                        .find(|b| !b.is_remote && b.name == name)
                        .map(|b| b.oid)
                });
            self.jump_to_commit(oid, name);
            return;
        }
        if let Some(name) = key.strip_prefix("remote:") {
            let oid = self
                .active_focus()
                .and_then(|t| {
                    t.branch_tips
                        .iter()
                        .find(|b| b.is_remote && b.name == name)
                        .map(|b| b.oid)
                });
            self.jump_to_commit(oid, name);
            return;
        }
        if let Some(name) = key.strip_prefix("tag:") {
            let oid = self
                .active_focus()
                .and_then(|t| t.tags.iter().find(|t| t.name == name).map(|t| t.oid));
            self.jump_to_commit(oid, name);
            return;
        }
        if key.starts_with("stash:") {
            // Stash actions (Apply / Pop / Drop) are right-click only.
            // `StashEntry` doesn't carry the WIP commit OID, so a row
            // click has no jump-to-commit target — treat it as a no-op.
            return;
        }

        // Breadcrumb segment click — pop until exactly `depth` entries
        // remain on the nav stack. depth=0 returns to root; the deepest
        // segment isn't routed (it's the view you're already on).
        if let Some(depth_str) = key.strip_prefix("nav:exit_to:") {
            if let Ok(depth) = depth_str.parse::<usize>()
                && let Some(tab) = self.active_mut()
            {
                tab.exit_to_depth(depth);
            }
            return;
        }

        // Submodule sibling switch: pop the current view + drill into
        // a sibling at the same depth. Routed from the sibling strip
        // at the bottom of the focused body.
        if let Some(path) = key.strip_prefix("submodule:switch:") {
            let path = path.to_string();
            if let Some(tab) = self.active_mut() {
                match tab.switch_sibling_submodule(&path) {
                    Ok(()) => {
                        self.toasts
                            .push(ToastSpec::success(format!("Switched to {path}")));
                    }
                    Err(e) => {
                        self.toasts.push(ToastSpec::error(format!(
                            "Couldn't switch to {path}: {e}"
                        )));
                    }
                }
            }
            return;
        }

        // Submodule drill-down: push the submodule onto the active
        // tab's nav_stack. Path is relative to the *focused* worktree,
        // resolved inside RepoTab::enter_submodule so nested drill-down
        // works without the caller knowing the depth.
        if let Some(path) = key.strip_prefix("submodule:open:") {
            let path = path.to_string();
            if let Some(tab) = self.active_mut() {
                match tab.enter_submodule(&path) {
                    Ok(()) => {
                        self.toasts.push(ToastSpec::success(format!(
                            "Entered {path}"
                        )));
                    }
                    Err(e) => {
                        self.toasts.push(ToastSpec::error(format!(
                            "Couldn't open submodule {path}: {e}"
                        )));
                    }
                }
            }
            return;
        }

        match key {
            "open_repo" => self.open_repo_dialog(),
            "new_branch" => self.open_branch_modal(),
            "new_tag" => self.open_tag_modal(),
            "new_worktree" => self.open_worktree_modal(),
            "pull_options" => self.open_pull_picker(),
            "push_options" => self.open_push_picker(),
            "ai_generate" => self.generate_commit_message_via_ai(),
            "welcome:clone" => {
                let mut form = CloneForm::default();
                form.dest = std::env::var("HOME").unwrap_or_default();
                self.active_modal = Some(ActiveModal::Clone(form));
            }
            "close_tab" => self.close_tab(self.active_tab),
            "fetch" => self.fetch(),
            "pull" => self.pull(),
            "push" => self.push(),
            "commit" => self.commit(),
            "stage_all" => self.stage_all(),
            "unstage_all" => self.unstage_all(),
            "settings" => self.active_modal = Some(ActiveModal::Settings),
            "toggle_shortcut_bar" => {
                self.shortcut_bar_visible = !self.shortcut_bar_visible;
            }
            "history:search_open" => {
                if let Some(tab) = self.active_focus_mut() {
                    tab.history_search_open = true;
                }
            }
            "details:copy_sha" => {
                if let Some(oid) = self.active_focus().and_then(|t| t.selected_commit) {
                    let sha = oid.to_string();
                    match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(sha.clone())) {
                        Ok(()) => self
                            .toasts
                            .push(ToastSpec::success(format!("Copied {}", &sha[..7]))),
                        Err(e) => self
                            .toasts
                            .push(ToastSpec::error(format!("Clipboard: {e}"))),
                    }
                }
            }
            _ => {}
        }
    }

    fn close_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        self.tabs.remove(idx);
        if self.active_tab >= self.tabs.len() && !self.tabs.is_empty() {
            self.active_tab = self.tabs.len() - 1;
        }
    }

    /// Open a native file picker (via `rfd`) and add the chosen
    /// directory as a new repo tab. Async polling for picker results
    /// can come later; today we block on the picker call (the picker
    /// runs in its own OS process / window so the user is interacting
    /// with that, not the frozen UI).
    fn open_repo_dialog(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("Open repository")
            .pick_folder();
        let Some(path) = picked else { return };
        self.open_repo_path(path);
    }

    /// Open a known path as a new repo tab. Shared between the file
    /// picker (`open_repo_dialog`) and the welcome view's recent-repos
    /// list. On success the path is promoted in `recent_repos` and the
    /// config is persisted; on failure an Error modal surfaces the
    /// reason.
    fn open_repo_path(&mut self, path: std::path::PathBuf) {
        match RepoTab::open(&path) {
            Ok(tab) => {
                self.tabs.push(tab);
                self.active_tab = self.tabs.len() - 1;
                let path_str = path.to_string_lossy().into_owned();
                if let Err(e) = self.config.add_recent_repo(&path_str) {
                    self.toasts
                        .push(ToastSpec::error(format!("Save recent failed: {e}")));
                }
                self.toasts
                    .push(ToastSpec::success(format!("Opened {}", path.display())));
            }
            Err(e) => {
                self.active_modal = Some(ActiveModal::Error {
                    title: "Open failed".to_string(),
                    body: format!("Could not open {}: {e}", path.display()),
                });
            }
        }
    }

    /// Open the sidebar context menu when the user right-clicks on an
    /// item. Returns true if the event was handled.
    fn handle_secondary_click(&mut self, event: &UiEvent) -> bool {
        let Some(route) = event.route() else {
            return false;
        };
        let target = if let Some(idx_str) = route.strip_prefix("commit:") {
            let Ok(idx) = idx_str.parse::<usize>() else {
                return false;
            };
            let Some(oid) = self
                .active_focus()
                .and_then(|t| t.commits.get(idx).map(|c| c.id))
            else {
                return false;
            };
            ContextTarget::Commit(oid)
        } else {
            let Some(t) = parse_sidebar_target(route) else {
                return false;
            };
            t
        };
        let Some(pos) = event.pointer_pos() else {
            return false;
        };
        self.context_menu = Some(ContextMenuState { pos, target });
        true
    }

    /// Handle modal-only routes. Returns true if the key was a modal
    /// route (settings:* / modal:* / scrim dismiss / context-menu :ctx)
    /// so the caller can short-circuit.
    fn handle_modal_route(&mut self, key: &str) -> bool {
        // Scrim dismiss — match the specific overlay so we don't close
        // a modal when the user clicked outside the context menu (or
        // vice versa).
        if let Some(scope) = key.strip_suffix(":dismiss") {
            if scope == SIDEBAR_CTX_KEY {
                self.context_menu = None;
            } else if scope.starts_with("modal:") {
                self.active_modal = None;
            }
            return true;
        }

        // Context-menu actions (`ctx:action`).
        if let Some(action) = key.strip_prefix("ctx:") {
            self.handle_context_action(action);
            return true;
        }

        if let Some(rest) = key.strip_prefix("settings:") {
            self.handle_settings_route(rest);
            return true;
        }

        if key.starts_with("modal:open_repo:") {
            self.handle_open_repo_route(key);
            return true;
        }
        if key.starts_with("clone:") {
            self.handle_clone_route(key);
            return true;
        }
        if key.starts_with("token:") {
            self.handle_token_route(key);
            return true;
        }
        // Only intercept `branch:` keys when the Branch modal is open —
        // otherwise sidebar `branch:<name>` clicks (handled in
        // handle_action's jump-to-commit path) would be shadowed.
        if matches!(self.active_modal, Some(ActiveModal::Branch { .. }))
            && key.starts_with("branch:")
        {
            self.handle_branch_route(key);
            return true;
        }
        // Same gating for `tag:` — sidebar `tag:<name>` jump-to-commit
        // clicks must still reach the handle_action path when the modal
        // is closed.
        if matches!(self.active_modal, Some(ActiveModal::Tag { .. }))
            && key.starts_with("tag:")
        {
            self.handle_tag_route(key);
            return true;
        }
        // `pull:` keys (rebase toggle, execute, source radio fallthrough)
        // are only meaningful while the picker is open. The bare "pull"
        // header-button route stays intact since it doesn't carry the colon.
        if matches!(self.active_modal, Some(ActiveModal::PullPicker { .. }))
            && key.starts_with("pull:")
        {
            self.handle_pull_route(key);
            return true;
        }
        // Same gating for `push:` — bare "push" header button keeps its
        // default behavior; modal-only fields and execute route through here.
        if matches!(self.active_modal, Some(ActiveModal::PushPicker { .. }))
            && key.starts_with("push:")
        {
            self.handle_push_route(key);
            return true;
        }
        // Same gating for `worktree:` — `wt_select:tab:<path>` is the
        // worktree pill bar's switch-route and doesn't carry the
        // `worktree:` prefix, so it stays reachable when the modal is
        // closed.
        if matches!(self.active_modal, Some(ActiveModal::Worktree { .. }))
            && key.starts_with("worktree:")
        {
            self.handle_worktree_route(key);
            return true;
        }

        match key {
            "modal:confirm:cancel" => {
                self.active_modal = None;
                true
            }
            "modal:confirm:ok" => {
                if let Some(ActiveModal::Confirm { action, .. }) = self.active_modal.take() {
                    self.run_confirm_action(action);
                }
                true
            }
            "modal:error:close" => {
                self.active_modal = None;
                true
            }
            "modal:clone:cancel" => {
                self.active_modal = None;
                true
            }
            "modal:token:close" => {
                self.active_modal = None;
                true
            }
            "modal:branch:cancel" => {
                self.active_modal = None;
                true
            }
            "modal:tag:cancel" => {
                self.active_modal = None;
                true
            }
            "modal:pull:cancel" => {
                self.active_modal = None;
                true
            }
            "modal:push:cancel" => {
                self.active_modal = None;
                true
            }
            "modal:worktree:cancel" => {
                self.active_modal = None;
                true
            }
            _ => false,
        }
    }

    fn handle_branch_route(&mut self, key: &str) {
        match key {
            "branch:checkout" => {
                if let Some(ActiveModal::Branch { form, .. }) = &mut self.active_modal {
                    form.checkout = !form.checkout;
                }
            }
            "branch:create" => self.create_branch_from_modal(),
            _ => {}
        }
    }

    /// Apply the create-branch modal's form: validate the name, call
    /// GitRepo::create_branch_at, optionally checkout, refresh, and
    /// close the modal. Errors surface as toasts so the user can edit
    /// and retry without retyping.
    fn create_branch_from_modal(&mut self) {
        let (name, target, checkout) = match &self.active_modal {
            Some(ActiveModal::Branch { form, target }) => {
                (form.name.trim().to_string(), *target, form.checkout)
            }
            _ => return,
        };
        if name.is_empty() {
            self.toasts
                .push(ToastSpec::warning("Branch name is empty"));
            return;
        }
        let Some(tab) = self.active_focus_mut() else { return };
        match tab.repo.create_branch_at(&name, target) {
            Ok(()) => {
                let mut msg = format!("Created branch {name}");
                if checkout {
                    if let Some(view) = tab.active_view_mut() {
                        match view.repo.checkout_branch(&name) {
                            Ok(()) => {
                                msg.push_str(" + checked out");
                            }
                            Err(e) => {
                                self.toasts.push(ToastSpec::error(format!(
                                    "Created {name}, but checkout failed: {e}"
                                )));
                                self.active_modal = None;
                                let proxy = self.proxy.clone();
                                let show_orphans = self.config.show_orphaned_commits;
                                if let Some(t) = self.active_focus_mut() {
                                    t.request_state_refresh(proxy.as_ref(), show_orphans);
                                }
                                return;
                            }
                        }
                    }
                }
                self.toasts.push(ToastSpec::success(msg));
                self.active_modal = None;
                let proxy = self.proxy.clone();
                let show_orphans = self.config.show_orphaned_commits;
                if let Some(t) = self.active_focus_mut() {
                    t.request_state_refresh(proxy.as_ref(), show_orphans);
                }
            }
            Err(e) => {
                self.toasts
                    .push(ToastSpec::error(format!("Create branch failed: {e}")));
            }
        }
    }

    /// Open the create-branch modal. Resolves the target OID from the
    /// focused tab — selected commit if one is open, otherwise the
    /// active worktree's HEAD. Bails with a toast when neither is
    /// available (effectively-bare repo with no selection).
    fn open_branch_modal(&mut self) {
        let Some(focus) = self.active_focus() else {
            return;
        };
        let target = focus
            .selected_commit
            .or_else(|| focus.active_view().and_then(|v| v.head_oid));
        let Some(target) = target else {
            self.toasts.push(ToastSpec::warning(
                "Select a commit (or check out a worktree) before creating a branch",
            ));
            return;
        };
        self.active_modal = Some(ActiveModal::Branch {
            form: BranchForm {
                name: String::new(),
                checkout: true,
            },
            target,
        });
    }

    fn handle_tag_route(&mut self, key: &str) {
        if key == "tag:create" {
            self.create_tag_from_modal();
        }
    }

    fn create_tag_from_modal(&mut self) {
        let (name, target) = match &self.active_modal {
            Some(ActiveModal::Tag { form, target }) => {
                (form.name.trim().to_string(), *target)
            }
            _ => return,
        };
        if name.is_empty() {
            self.toasts.push(ToastSpec::warning("Tag name is empty"));
            return;
        }
        let Some(tab) = self.active_focus_mut() else { return };
        match tab.repo.create_tag(&name, target) {
            Ok(()) => {
                self.toasts
                    .push(ToastSpec::success(format!("Created tag {name}")));
                self.active_modal = None;
                let proxy = self.proxy.clone();
                let show_orphans = self.config.show_orphaned_commits;
                if let Some(t) = self.active_focus_mut() {
                    t.request_state_refresh(proxy.as_ref(), show_orphans);
                }
            }
            Err(e) => {
                self.toasts
                    .push(ToastSpec::error(format!("Create tag failed: {e}")));
            }
        }
    }

    fn open_tag_modal(&mut self) {
        let Some(focus) = self.active_focus() else {
            return;
        };
        let target = focus
            .selected_commit
            .or_else(|| focus.active_view().and_then(|v| v.head_oid));
        let Some(target) = target else {
            self.toasts.push(ToastSpec::warning(
                "Select a commit (or check out a worktree) before creating a tag",
            ));
            return;
        };
        self.active_modal = Some(ActiveModal::Tag {
            form: TagForm::default(),
            target,
        });
    }

    fn handle_pull_route(&mut self, key: &str) {
        match key {
            "pull:rebase" => {
                if let Some(ActiveModal::PullPicker { form, .. }) = &mut self.active_modal {
                    form.rebase = !form.rebase;
                }
            }
            "pull:execute" => self.pull_from_modal(),
            // `pull:source:radio:<value>` is folded by `radio::apply_event`
            // up in `on_event`; nothing to do here for those.
            _ => {}
        }
    }

    /// Apply the pull-picker form: pull from the selected source with
    /// or without `--rebase`. Picks the right git-cli helper and parks
    /// the receiver on the same `pull_op` slot as the default Pull
    /// button so the header progress affordance and post-op refresh
    /// don't need a separate code path.
    fn pull_from_modal(&mut self) {
        let (source, rebase) = match &self.active_modal {
            Some(ActiveModal::PullPicker { form, .. }) => {
                (form.source.trim().to_string(), form.rebase)
            }
            _ => return,
        };
        let Some((remote, branch)) = source.split_once('/') else {
            self.toasts.push(ToastSpec::warning(
                "Source must be of the form <remote>/<branch>",
            ));
            return;
        };
        let (remote, branch) = (remote.to_string(), branch.to_string());
        let Some((wd, proxy)) = self.prepare_remote_op(AsyncKind::Pull, true) else {
            return;
        };
        let rx = if rebase {
            crate::git::pull_rebase_async(wd, remote.clone(), branch.clone(), proxy)
        } else {
            crate::git::pull_remote_async(wd, remote.clone(), branch.clone(), proxy)
        };
        let Some(tab) = self.active_focus_mut() else { return };
        let label = if rebase {
            format!("{remote}/{branch} (rebase)")
        } else {
            format!("{remote}/{branch}")
        };
        tab.pull_op = Some(TimedOp::new(rx, label.clone()));
        self.toasts
            .push(ToastSpec::info(format!("Pulling {label}…")));
        self.active_modal = None;
    }

    /// Open the pull picker. Sources come from the focused tab's
    /// remote-tracking branch list. The default selection is the
    /// current branch's upstream when one is configured, falling
    /// back to `<default_remote>/<current_branch>` when that label
    /// happens to be in the list. Empty source list (no remotes /
    /// no remote-tracking branches) opens an inert modal — handler
    /// gates the Pull button until a row is selected.
    fn open_pull_picker(&mut self) {
        let Some(focus) = self.active_focus() else {
            return;
        };
        if !focus.repo.has_remotes() {
            self.toasts.push(ToastSpec::error(
                "No remotes configured for this repository",
            ));
            return;
        }
        let mut sources: Vec<String> = focus
            .remote_branches()
            .into_iter()
            .flat_map(|(remote, branches)| {
                branches
                    .into_iter()
                    .map(move |b| format!("{remote}/{b}"))
            })
            .collect();
        sources.sort();
        if sources.is_empty() {
            self.toasts.push(ToastSpec::warning(
                "No remote-tracking branches yet — fetch first",
            ));
            return;
        }
        let default = focus
            .branch_tips
            .iter()
            .find(|b| !b.is_remote && b.is_head)
            .and_then(|b| b.upstream.clone())
            .or_else(|| {
                let remote = focus.repo.default_remote().ok()?;
                let branch = focus.current_branch();
                if branch.is_empty() {
                    None
                } else {
                    Some(format!("{remote}/{branch}"))
                }
            })
            .filter(|s| sources.contains(s))
            .unwrap_or_default();
        self.active_modal = Some(ActiveModal::PullPicker {
            form: PullForm {
                source: default,
                rebase: false,
            },
            sources,
        });
    }

    fn handle_push_route(&mut self, key: &str) {
        match key {
            "push:force" => {
                if let Some(ActiveModal::PushPicker { form, .. }) = &mut self.active_modal {
                    form.force_with_lease = !form.force_with_lease;
                }
            }
            "push:set_upstream" => {
                if let Some(ActiveModal::PushPicker { form, .. }) = &mut self.active_modal {
                    form.set_upstream = !form.set_upstream;
                }
            }
            "push:tags" => {
                if let Some(ActiveModal::PushPicker { form, .. }) = &mut self.active_modal {
                    form.include_tags = !form.include_tags;
                }
            }
            "push:execute" => self.push_from_modal(),
            // `push:remote:radio:<value>` and `push:branch` text edits are
            // folded by `radio::apply_event` / `text_input::apply_event`
            // up in `on_event`; nothing to do here for those.
            _ => {}
        }
    }

    /// Apply the push-picker form: push the chosen branch to the chosen
    /// remote with the requested flag combination. Routes through
    /// `git::push_with_options_async`, parking the receiver on the same
    /// `push_op` slot as the bare Push button so the header progress
    /// affordance and post-op refresh don't need a separate code path.
    fn push_from_modal(&mut self) {
        let (remote, branch, force, upstream, tags) = match &self.active_modal {
            Some(ActiveModal::PushPicker { form, .. }) => (
                form.remote.trim().to_string(),
                form.branch.trim().to_string(),
                form.force_with_lease,
                form.set_upstream,
                form.include_tags,
            ),
            _ => return,
        };
        if remote.is_empty() || branch.is_empty() {
            self.toasts
                .push(ToastSpec::warning("Push: remote and branch are required"));
            return;
        }
        let Some((wd, proxy)) = self.prepare_remote_op(AsyncKind::Push, true) else {
            return;
        };
        let rx = crate::git::push_with_options_async(
            wd,
            remote.clone(),
            branch.clone(),
            force,
            upstream,
            tags,
            proxy,
        );
        let Some(tab) = self.active_focus_mut() else { return };
        let mut suffix = Vec::new();
        if force {
            suffix.push("force");
        }
        if upstream {
            suffix.push("set-upstream");
        }
        if tags {
            suffix.push("tags");
        }
        let label = if suffix.is_empty() {
            format!("{branch} → {remote}")
        } else {
            format!("{branch} → {remote} ({})", suffix.join(", "))
        };
        tab.push_op = Some(TimedOp::new(rx, label.clone()));
        self.toasts
            .push(ToastSpec::info(format!("Pushing {label}…")));
        self.active_modal = None;
    }

    /// Open the push-options picker. Pre-fills remote with the focused
    /// tab's `default_remote()` (when present in the remote list) and
    /// branch with the current branch shorthand. Detached HEAD opens
    /// with an empty branch field, which keeps the Push button disabled
    /// until the user types one.
    fn open_push_picker(&mut self) {
        let Some(focus) = self.active_focus() else {
            return;
        };
        if !focus.repo.has_remotes() {
            self.toasts.push(ToastSpec::error(
                "No remotes configured for this repository",
            ));
            return;
        }
        let mut remotes = focus.repo.remote_names();
        remotes.sort();
        if remotes.is_empty() {
            self.toasts
                .push(ToastSpec::warning("No remotes configured"));
            return;
        }
        let default_remote = focus
            .repo
            .default_remote()
            .ok()
            .filter(|r| remotes.contains(r))
            .unwrap_or_else(|| remotes[0].clone());
        let branch = focus.current_branch().to_string();
        self.active_modal = Some(ActiveModal::PushPicker {
            form: PushForm {
                remote: default_remote,
                branch,
                force_with_lease: false,
                set_upstream: false,
                include_tags: false,
            },
            remotes,
        });
    }

    fn handle_worktree_route(&mut self, key: &str) {
        match key {
            "worktree:detached" => {
                if let Some(ActiveModal::Worktree { form }) = &mut self.active_modal {
                    form.detached = !form.detached;
                }
            }
            "worktree:submodules" => {
                if let Some(ActiveModal::Worktree { form }) = &mut self.active_modal {
                    form.init_submodules = !form.init_submodules;
                }
            }
            "worktree:create" => self.create_worktree_from_modal(),
            _ => {}
        }
    }

    /// Apply the worktree-creation form: spawn `git worktree add` (with
    /// optional `--detach` and submodule init follow-up) on the worktree
    /// helper that already chains those steps. Routes through the
    /// mutation_op slot — the post-op refresh picks up the new worktree
    /// in `RepoTab::merge_worktree_views`.
    fn create_worktree_from_modal(&mut self) {
        let (path, source, detached, init_submodules) = match &self.active_modal {
            Some(ActiveModal::Worktree { form }) => (
                form.path.trim().to_string(),
                form.source.trim().to_string(),
                form.detached,
                form.init_submodules,
            ),
            _ => return,
        };
        if path.is_empty() {
            self.toasts.push(ToastSpec::warning("Path is empty"));
            return;
        }
        if source.is_empty() {
            self.toasts.push(ToastSpec::warning("Source is empty"));
            return;
        }
        let Some((wd, proxy)) = self.prepare_remote_op(AsyncKind::Mutation, false) else {
            return;
        };
        let rx = crate::git::create_worktree_with_post_steps_async(
            wd,
            path.clone(),
            source.clone(),
            detached,
            init_submodules,
            false,
            proxy,
        );
        let Some(tab) = self.active_focus_mut() else { return };
        let label = if detached {
            format!("worktree {path} (detached @ {source})")
        } else {
            format!("worktree {path} ({source})")
        };
        tab.mutation_op = Some(TimedOp::new(rx, label.clone()));
        self.toasts
            .push(ToastSpec::info(format!("Creating {label}…")));
        self.active_modal = None;
    }

    /// Open the create-worktree modal. Pre-fills the path with a
    /// sibling-of-repo default named after the source ref, and the
    /// source with the current branch when checked out.
    fn open_worktree_modal(&mut self) {
        let Some(focus) = self.active_focus() else {
            return;
        };
        let parent = focus
            .repo
            .git_command_dir()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let source = focus.current_branch().to_string();
        let path_default = if source.is_empty() {
            String::new()
        } else {
            // Sanitize slashes so a branch like "feature/x" doesn't
            // accidentally create a nested directory under parent.
            let dir_name = source.replace('/', "-");
            parent.join(dir_name).to_string_lossy().to_string()
        };
        self.active_modal = Some(ActiveModal::Worktree {
            form: WorktreeForm {
                path: path_default,
                source,
                detached: false,
                init_submodules: false,
            },
        });
    }

    /// Spawn the AI-commit-message worker for the focused tab's
    /// active worktree. Bails with a toast if no proxy / no active
    /// worktree / generation already running. The Generate button
    /// in the staging well disables on those same conditions, so
    /// the toasts here cover only edge cases (e.g. losing the proxy).
    fn generate_commit_message_via_ai(&mut self) {
        let Some(proxy) = self.proxy.clone() else {
            self.toasts.push(ToastSpec::error(
                "AI generate unavailable: event loop proxy missing",
            ));
            return;
        };
        let provider = crate::ai::AiProvider::from_config(&self.config.ai_provider);
        let Some(tab) = self.active_focus_mut() else {
            return;
        };
        if tab.ai_op.is_some() {
            self.toasts
                .push(ToastSpec::info("AI generate already in progress"));
            return;
        }
        let Some(view) = tab.active_view() else {
            self.toasts.push(ToastSpec::warning(
                "No active worktree to generate a message for",
            ));
            return;
        };
        let target_path = view.path.clone();
        let workdir = view.repo.git_command_dir();
        let branch = tab.current_branch().to_string();
        let rx = crate::ai::spawn_generate_async(workdir, branch, provider, proxy);
        tab.ai_op = Some(crate::repo_tab::AiOp {
            rx,
            started: std::time::Instant::now(),
            target_path,
        });
        self.toasts
            .push(ToastSpec::info("Generating commit message…"));
    }

    /// Routes for the Open-repo modal opened by the tab-bar `+`. Each
    /// path closes the modal first so a new modal (Clone, Error from
    /// `open_repo_path`) can replace it cleanly without a one-frame gap
    /// of two dialogs being live.
    fn handle_open_repo_route(&mut self, key: &str) {
        match key {
            "modal:open_repo:cancel" => {
                self.active_modal = None;
            }
            "modal:open_repo:browse" => {
                self.active_modal = None;
                self.open_repo_dialog();
            }
            "modal:open_repo:clone" => {
                let mut form = CloneForm::default();
                form.dest = std::env::var("HOME").unwrap_or_default();
                self.active_modal = Some(ActiveModal::Clone(form));
            }
            other => {
                if let Some(idx_str) = other.strip_prefix("modal:open_repo:recent:")
                    && let Ok(idx) = idx_str.parse::<usize>()
                    && let Some(path) = self.config.recent_repos.get(idx).cloned()
                {
                    self.active_modal = None;
                    self.open_repo_path(std::path::PathBuf::from(path));
                }
            }
        }
    }

    fn handle_clone_route(&mut self, key: &str) {
        match key {
            "clone:bare" => {
                if let Some(ActiveModal::Clone(form)) = &mut self.active_modal {
                    form.bare = !form.bare;
                }
            }
            "clone:browse" => {
                let picked = rfd::FileDialog::new()
                    .set_title("Choose destination")
                    .pick_folder();
                if let Some(p) = picked
                    && let Some(ActiveModal::Clone(form)) = &mut self.active_modal
                {
                    form.dest = p.to_string_lossy().to_string();
                }
            }
            "clone:start" => self.start_clone(),
            _ => {}
        }
    }

    fn handle_token_route(&mut self, key: &str) {
        match key {
            "token:github:edit" => {
                if let Some(ActiveModal::Token(form)) = &mut self.active_modal {
                    form.editing_github = true;
                    form.github_input.clear();
                }
            }
            "token:github:cancel" => {
                if let Some(ActiveModal::Token(form)) = &mut self.active_modal {
                    form.editing_github = false;
                    form.github_input.clear();
                }
            }
            "token:github:save" => {
                let value = if let Some(ActiveModal::Token(form)) = &self.active_modal {
                    form.github_input.trim().to_string()
                } else {
                    return;
                };
                if value.is_empty() {
                    self.toasts
                        .push(ToastSpec::warning("Token is empty — leaving unchanged"));
                    return;
                }
                if token_store::set_github_token(&value) {
                    self.toasts.push(ToastSpec::success("GitHub token saved"));
                    if let Some(ActiveModal::Token(form)) = &mut self.active_modal {
                        form.editing_github = false;
                        form.github_input.clear();
                    }
                } else {
                    self.toasts
                        .push(ToastSpec::error("Couldn't write to keychain"));
                }
            }
            "token:github:clear" => {
                if token_store::delete_github_token() {
                    self.toasts.push(ToastSpec::success("GitHub token cleared"));
                } else {
                    self.toasts
                        .push(ToastSpec::error("Couldn't clear keychain entry"));
                }
            }
            other => {
                if let Some(host) = other.strip_prefix("token:gitlab:edit:") {
                    if let Some(ActiveModal::Token(form)) = &mut self.active_modal {
                        form.gitlab_inputs.insert(host.to_string(), String::new());
                    }
                } else if let Some(host) = other.strip_prefix("token:gitlab:cancel:") {
                    if let Some(ActiveModal::Token(form)) = &mut self.active_modal {
                        form.gitlab_inputs.remove(host);
                    }
                } else if let Some(host) = other.strip_prefix("token:gitlab:save:") {
                    let value = if let Some(ActiveModal::Token(form)) = &self.active_modal {
                        form.gitlab_inputs
                            .get(host)
                            .map(|s| s.trim().to_string())
                            .unwrap_or_default()
                    } else {
                        return;
                    };
                    if value.is_empty() {
                        self.toasts.push(ToastSpec::warning(
                            "Token is empty — leaving unchanged",
                        ));
                        return;
                    }
                    if token_store::set_gitlab_token(host, &value) {
                        self.toasts
                            .push(ToastSpec::success(format!("GitLab token saved ({host})")));
                        if let Some(ActiveModal::Token(form)) = &mut self.active_modal {
                            form.gitlab_inputs.remove(host);
                        }
                    } else {
                        self.toasts
                            .push(ToastSpec::error("Couldn't write to keychain"));
                    }
                } else if let Some(host) = other.strip_prefix("token:gitlab:clear:") {
                    if token_store::delete_gitlab_token(host) {
                        self.toasts
                            .push(ToastSpec::success(format!("GitLab token cleared ({host})")));
                    } else {
                        self.toasts
                            .push(ToastSpec::error("Couldn't clear keychain entry"));
                    }
                }
            }
        }
    }

    /// Spawn a `git clone` thread for the URL/dest in the current
    /// Clone modal. Validates the inputs first, surfaces errors as
    /// toasts (rather than blocking the modal flow), and parks the
    /// receiver on `clone_op` for `poll_async_ops` to drain.
    fn start_clone(&mut self) {
        let Some(proxy) = self.proxy.clone() else {
            self.toasts
                .push(ToastSpec::error("Clone unavailable: event loop proxy missing"));
            return;
        };
        if self.clone_op.is_some() {
            self.toasts
                .push(ToastSpec::info("A clone is already in progress"));
            return;
        }
        let Some(ActiveModal::Clone(form)) = &self.active_modal else {
            return;
        };
        let url = form.url.trim().to_string();
        let dest = form.dest.trim().to_string();
        let bare = form.bare;
        if url.is_empty() {
            self.toasts.push(ToastSpec::warning("Repository URL is required"));
            return;
        }
        if dest.is_empty() {
            self.toasts
                .push(ToastSpec::warning("Destination directory is required"));
            return;
        }
        let dest_path = std::path::PathBuf::from(&dest);
        let dest_label = dest_path.display().to_string();
        let rx = crate::git::clone_async(url.clone(), dest_path, bare, proxy);
        self.clone_op = Some(CloneOp {
            rx,
            started: std::time::Instant::now(),
            dest_label: dest_label.clone(),
        });
        self.toasts
            .push(ToastSpec::info(format!("Cloning into {dest_label}\u{2026}")));
    }

    fn handle_context_action(&mut self, action: &str) {
        let Some(state) = self.context_menu.take() else {
            return;
        };
        match (action, state.target) {
            ("checkout", ContextTarget::LocalBranch(name)) => {
                self.run_op("Checkout", |t| t.repo.checkout_branch(&name));
            }
            ("checkout", ContextTarget::RemoteBranch { remote, branch }) => {
                self.run_op("Checkout", |t| {
                    t.repo.checkout_remote_branch(&remote, &branch)
                });
            }
            ("merge", ContextTarget::LocalBranch(name)) => {
                self.merge_branch(name);
            }
            ("merge", ContextTarget::RemoteBranch { remote, branch }) => {
                self.merge_branch(format!("{remote}/{branch}"));
            }
            ("rebase", ContextTarget::LocalBranch(name)) => {
                self.rebase_onto(name);
            }
            ("rebase", ContextTarget::RemoteBranch { remote, branch }) => {
                self.rebase_onto(format!("{remote}/{branch}"));
            }
            ("delete", ContextTarget::LocalBranch(name)) => {
                self.active_modal = Some(ActiveModal::Confirm {
                    title: "Delete branch".to_string(),
                    body: format!("Delete local branch '{name}' permanently?"),
                    ok_label: "Delete".to_string(),
                    destructive: true,
                    action: ConfirmAction::DeleteBranch(name),
                });
            }
            ("delete", ContextTarget::Tag(name)) => {
                self.active_modal = Some(ActiveModal::Confirm {
                    title: "Delete tag".to_string(),
                    body: format!("Delete tag '{name}' permanently?"),
                    ok_label: "Delete".to_string(),
                    destructive: true,
                    action: ConfirmAction::DeleteTag(name),
                });
            }
            ("apply", ContextTarget::Stash(idx)) => {
                self.stash_apply(idx);
            }
            ("pop", ContextTarget::Stash(idx)) => {
                self.stash_pop(idx);
            }
            ("drop", ContextTarget::Stash(idx)) => {
                self.active_modal = Some(ActiveModal::Confirm {
                    title: "Drop stash".to_string(),
                    body: format!("Drop stash @{{{idx}}} permanently?"),
                    ok_label: "Drop".to_string(),
                    destructive: true,
                    action: ConfirmAction::DropStash(idx),
                });
            }
            ("copy_sha", ContextTarget::Commit(oid)) => {
                let sha = oid.to_string();
                match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(sha.clone())) {
                    Ok(()) => self
                        .toasts
                        .push(ToastSpec::success(format!("Copied {}", &sha[..7]))),
                    Err(e) => self
                        .toasts
                        .push(ToastSpec::error(format!("Clipboard: {e}"))),
                }
            }
            ("checkout", ContextTarget::Commit(oid)) => {
                self.run_op("Checkout", move |t| t.repo.checkout_commit_detached(oid));
            }
            ("reset_hard", ContextTarget::Commit(oid)) => {
                let short = oid.to_string()[..7].to_string();
                self.active_modal = Some(ActiveModal::Confirm {
                    title: "Reset hard".to_string(),
                    body: format!("Move HEAD to {short} and discard all changes in tracked files?"),
                    ok_label: "Reset".to_string(),
                    destructive: true,
                    action: ConfirmAction::ResetHard(oid),
                });
            }
            ("cherry_pick", ContextTarget::Commit(oid)) => {
                self.cherry_pick(oid);
            }
            ("revert", ContextTarget::Commit(oid)) => {
                self.revert(oid);
            }
            _ => {}
        }
    }

    fn handle_settings_route(&mut self, sub: &str) {
        match sub {
            "close" => self.active_modal = None,
            "avatars" => {
                self.config.avatars_enabled = !self.config.avatars_enabled;
                self.persist_config();
            }
            "shortcut_bar" => {
                self.shortcut_bar_visible = !self.shortcut_bar_visible;
                self.config.shortcut_bar_visible = self.shortcut_bar_visible;
                self.persist_config();
            }
            "clone" => {
                // Clone-from-Settings: pre-fill destination with $HOME so
                // first-time users land in a sensible default location.
                let mut form = CloneForm::default();
                form.dest = std::env::var("HOME").unwrap_or_default();
                self.active_modal = Some(ActiveModal::Clone(form));
            }
            "tokens" => {
                self.active_modal = Some(ActiveModal::Token(TokenForm::default()));
            }
            other => {
                if let Some(scale_str) = other.strip_prefix("row_size:")
                    && let Ok(scale) = scale_str.parse::<f32>()
                {
                    self.config.row_scale = scale;
                    self.persist_config();
                }
            }
        }
    }

    fn persist_config(&mut self) {
        if let Err(e) = self.config.save() {
            self.toasts
                .push(ToastSpec::error(format!("Save settings failed: {e}")));
        }
    }

    fn run_confirm_action(&mut self, action: ConfirmAction) {
        match action {
            ConfirmAction::CloseTab(idx) => self.close_tab(idx),
            ConfirmAction::DeleteBranch(name) => {
                self.run_op("Delete branch", |t| t.repo.delete_branch(&name));
            }
            ConfirmAction::DeleteTag(name) => {
                self.run_op("Delete tag", |t| t.repo.delete_tag(&name));
            }
            ConfirmAction::DropStash(idx) => {
                self.stash_drop(idx);
            }
            ConfirmAction::ResetHard(oid) => {
                self.run_op("Reset hard", move |t| {
                    t.repo.reset_to_commit(oid, git2::ResetType::Hard)
                });
            }
            ConfirmAction::ForcePush { remote, branch } => {
                self.force_push(remote, branch);
            }
            ConfirmAction::UpdateSubmodulePin { sm_path } => {
                self.stage_submodule_pin_update(&sm_path);
            }
        }
    }

    /// Stage the parent's submodule pointer at `sm_path`, then pop the
    /// drill-down stack one level so the user lands on the parent
    /// view with the pointer change ready to commit. `sm_path` is
    /// relative to the parent's worktree — exactly the form
    /// `index.add_path` expects.
    ///
    /// No-op if not drilled in (no parent to stage against). On
    /// success we surface a toast naming the submodule + new short
    /// SHA so the user sees the pointer landed.
    fn stage_submodule_pin_update(&mut self, sm_path: &str) {
        let Some(outer) = self.active_mut() else {
            return;
        };
        // Capture the new HEAD before we exit (the focused tab carries
        // it via active_view_tab().active_view()).
        let new_short = outer
            .active_view_tab()
            .active_view()
            .and_then(|v| v.head_oid)
            .map(|o| o.to_string()[..7].to_string())
            .unwrap_or_else(|| "?".to_string());
        // Pop first so the parent becomes the focused tab. Now stage
        // through the parent's active worktree's repo.
        if !outer.exit_submodule() {
            return;
        }
        let Some(parent_view) = outer.active_view_tab_mut().active_view_mut() else {
            self.toasts.push(ToastSpec::error(
                "Parent view has no active worktree — cannot stage pointer update",
            ));
            return;
        };
        match parent_view.repo.stage_file(sm_path) {
            Ok(()) => {
                // Refresh the parent so the staged pointer change shows
                // up in its staging well. Refresh on the OUTERMOST
                // (which is now the focused view since we popped).
                let proxy = self.proxy.clone();
                let show_orphans = self.config.show_orphaned_commits;
                if let Some(outer) = self.active_mut() {
                    outer.request_state_refresh(proxy.as_ref(), show_orphans);
                }
                self.toasts.push(ToastSpec::success(format!(
                    "Staged {sm_path} \u{2192} {new_short}"
                )));
            }
            Err(e) => {
                self.toasts.push(ToastSpec::error(format!(
                    "Couldn't stage {sm_path}: {e}"
                )));
            }
        }
    }

    /// `git push --force-with-lease <remote> <branch>`. Reached only via
    /// the rejected-push Confirm modal, so the user has explicitly opted
    /// in. Writes through the same `push_op` slot as a regular push, so
    /// the header progress affordance and the failure-handling path
    /// don't need a separate code path.
    fn force_push(&mut self, remote: String, branch: String) {
        let Some((wd, proxy)) = self.prepare_remote_op(AsyncKind::Push, true) else {
            return;
        };
        let rx = crate::git::push_force_async(wd, remote.clone(), branch.clone(), proxy);
        let Some(tab) = self.active_focus_mut() else { return };
        tab.push_op = Some(TimedOp::new(rx, format!("{branch} \u{2192} {remote} (force)")));
        self.toasts
            .push(ToastSpec::info(format!("Force-pushing {branch} to {remote}…")));
    }

    // -------------------------------------------------------------
    // Async op infrastructure.
    //
    // Each remote op spawns a `git` CLI thread (see `git/async_ops.rs`)
    // that sends a `RemoteOpResult` back over a channel and wakes the
    // event loop via `EventLoopProxy`. We poll the channels in
    // `before_build`; on completion, refresh the tab and surface
    // success via toast / failure via error modal.
    // -------------------------------------------------------------

    /// Drain all per-tab async slots plus the app-scoped clone slot;
    /// called once per frame from `before_build`. Visits every tab and
    /// every drilled-in level inside it, so background work in a
    /// non-foreground tab — or on a parent repo while the user has
    /// drilled into a submodule — still completes cleanly.
    pub fn poll_async_ops(&mut self) {
        for idx in 0..self.tabs.len() {
            // Outermost first.
            self.poll_async_ops_at(idx, None);
            // Then each drilled-in submodule level. Re-read depth each
            // iteration since the outer poll may have refreshed and
            // exit_submodule could (in principle) shrink the stack.
            let mut d = 0;
            while let Some(t) = self.tabs.get(idx)
                && d < t.nav_stack.len()
            {
                self.poll_async_ops_at(idx, Some(d));
                d += 1;
            }
        }
        self.poll_clone_op();
        self.poll_state_refreshes();
        self.poll_status_refreshes();
        self.poll_dirty_checks();
        self.poll_watcher_inits();
        self.poll_watcher_events();
        self.poll_status_safety_net();
        self.poll_ref_reconciliation();
        self.drain_ci_receivers();
        self.poll_ci_refresh();
        self.drain_diff_stats();
        self.trigger_diff_stats_fetches();
        self.drain_avatar_completions();
        self.request_visible_avatars();
    }

    /// Kick off the very first state refresh for any tab that hasn't
    /// had one attempted yet. Runs every frame from `before_build` —
    /// the per-tab `state_refresh_attempted` flag short-circuits after
    /// the first call so this is cheap on subsequent frames.
    ///
    /// Tabs constructed before the proxy was set (the startup path
    /// through `from_paths`) sit in an empty state until the first
    /// frame where `proxy` is available; that's also the first frame
    /// this runs. Tabs created later (via `open_repo_path`) trigger
    /// their refresh inline at construction time and skip this.
    fn trigger_initial_state_refreshes(&mut self) {
        let Some(proxy) = self.proxy.clone() else { return };
        let show_orphans = self.config.show_orphaned_commits;
        for tab in &mut self.tabs {
            if !tab.state_refresh_attempted {
                tab.trigger_state_refresh(&proxy, show_orphans);
            }
            for sub in &mut tab.nav_stack {
                if !sub.state_refresh_attempted {
                    sub.trigger_state_refresh(&proxy, show_orphans);
                }
            }
        }
    }

    /// Drain finished `RepoStateResult`s for every tab + drilled-in
    /// level. On a result, fold via [`RepoTab::apply_state_result`],
    /// surface any errors as toasts, then dispatch the produced effects
    /// (kick off diff-stats fanout, kick off per-entity dirty checks,
    /// flag the watcher for path updates).
    fn poll_state_refreshes(&mut self) {
        for tab_idx in 0..self.tabs.len() {
            // Outermost first, then each drilled-in level.
            self.poll_state_refresh_at(tab_idx, None);
            let mut d = 0;
            while let Some(t) = self.tabs.get(tab_idx)
                && d < t.nav_stack.len()
            {
                self.poll_state_refresh_at(tab_idx, Some(d));
                d += 1;
            }
        }
    }

    fn poll_state_refresh_at(&mut self, tab_idx: usize, depth: Option<usize>) {
        let result = {
            let Some(tab) = self.tab_at_mut(tab_idx, depth) else { return };
            let Some(rx) = tab.state_refresh_rx.take() else { return };
            match rx.try_recv() {
                Ok(result) => Some(result),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Worker still running — put the receiver back.
                    tab.state_refresh_rx = Some(rx);
                    None
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
            }
        };
        let Some(result) = result else { return };

        // Apply + collect effects in a scoped borrow.
        let effects = {
            let Some(tab) = self.tab_at_mut(tab_idx, depth) else { return };
            tab.apply_state_result(result)
        };

        for err in effects.errors {
            self.toasts.push(ToastSpec::error(err));
        }

        // Diff-stats fetch for the new commit set. The existing
        // `trigger_diff_stats_fetches` runs every poll anyway, so this
        // is informational — but kicking it now starts the fetch in
        // the same frame instead of waiting for the next.
        let Some(proxy) = self.proxy.clone() else { return };
        if let Some(tab) = self.tab_at_mut(tab_idx, depth) {
            tab.trigger_diff_stats_fetch(proxy.clone());
        }

        // Per-entity dirty-check fanout — one worker per submodule, one
        // per worktree. `tab_id` on each result lets the global drain
        // route back here even after a tab close / reopen.
        let (tab_id, repo_workdir) = {
            let Some(tab) = self.tab_at_mut(tab_idx, depth) else { return };
            (tab.id, tab.repo.workdir().map(|p| p.to_path_buf()))
        };
        self.dirty_checks_in_flight += crate::git_async::spawn_dirty_checks(
            tab_id,
            &effects.dirty_checks_submodules,
            &effects.dirty_checks_worktrees,
            repo_workdir,
            &self.dirty_check_tx,
            &proxy,
        );

        // Update the watcher's per-worktree watch set if the resolved
        // worktree list changed. The watcher's submodule exclusion list
        // is set at construction only — for now we don't update it
        // mid-life (legacy gap; new submodules added during a session
        // surface as WorkingTree events until the watcher is recreated).
        if effects.watcher_paths_changed
            && let Some(tab) = self.tab_at_mut(tab_idx, depth)
        {
            let common_dir = tab.repo.common_dir().to_path_buf();
            let worktrees = tab.worktrees.clone();
            if let Some(w) = tab.watcher.as_mut() {
                w.update_worktree_watches(&worktrees, &common_dir);
            }
        }

        // Trigger watcher init once we have submodule paths from the
        // first state-refresh result. Subsequent calls short-circuit
        // (idempotent on `watcher.is_some() || watcher_init_rx.is_some()`).
        if let Some(tab) = self.tab_at_mut(tab_idx, depth) {
            tab.trigger_watcher_init(&proxy);
        }
    }

    /// Drain finished watcher init results for every tab + drilled-in
    /// level. On success, populate `watcher` + `watcher_rx`; on
    /// failure, surface a toast and leave the slots `None` (the safety
    /// nets still cover refresh).
    fn poll_watcher_inits(&mut self) {
        for tab_idx in 0..self.tabs.len() {
            self.poll_watcher_init_at(tab_idx, None);
            let mut d = 0;
            while let Some(t) = self.tabs.get(tab_idx)
                && d < t.nav_stack.len()
            {
                self.poll_watcher_init_at(tab_idx, Some(d));
                d += 1;
            }
        }
    }

    fn poll_watcher_init_at(&mut self, tab_idx: usize, depth: Option<usize>) {
        let result = {
            let Some(tab) = self.tab_at_mut(tab_idx, depth) else { return };
            let Some(rx) = tab.watcher_init_rx.take() else { return };
            match rx.try_recv() {
                Ok(result) => Some(result),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tab.watcher_init_rx = Some(rx);
                    None
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
            }
        };
        let Some(result) = result else { return };
        match result {
            Ok((watcher, watcher_rx)) => {
                if let Some(tab) = self.tab_at_mut(tab_idx, depth) {
                    tab.watcher = Some(watcher);
                    tab.watcher_rx = Some(watcher_rx);
                }
            }
            Err(e) => {
                self.toasts.push(ToastSpec::error(format!(
                    "Filesystem watcher failed to initialize: {e}"
                )));
            }
        }
    }

    /// Drain debounced `FsChangeKind` events for every tab + drilled-in
    /// level. Coalesces a burst of events to its highest-priority kind
    /// (so a stream of working-tree edits + a single git-commit event
    /// dispatches to GitMetadata, not WorkingTree). Per-kind dispatch:
    ///
    /// - `WorkingTree` → trigger working-dir status refresh + per-
    ///   worktree dirty check fanout
    /// - `GitMetadata` → reopen repo handles (libgit2 cache bypass) +
    ///   trigger full state refresh
    /// - `WorktreeStructure` → same as `GitMetadata` plus the next
    ///   state refresh's `watcher_paths_changed` effect updates the
    ///   watch set
    fn poll_watcher_events(&mut self) {
        let Some(proxy) = self.proxy.clone() else { return };
        let show_orphans = self.config.show_orphaned_commits;
        for tab_idx in 0..self.tabs.len() {
            self.dispatch_watcher_events_at(tab_idx, None, &proxy, show_orphans);
            let mut d = 0;
            while let Some(t) = self.tabs.get(tab_idx)
                && d < t.nav_stack.len()
            {
                self.dispatch_watcher_events_at(tab_idx, Some(d), &proxy, show_orphans);
                d += 1;
            }
        }
    }

    /// 30 s safety-net status refresh on the active tab. Catches the
    /// case where a watcher event was dropped (inotify queue overflow,
    /// filesystem races, NFS-style fakery) by re-querying status
    /// unconditionally on a long interval. Cheap relative to a full
    /// state refresh — just the working-dir status walk on the active
    /// view's repo.
    ///
    /// Background tabs aren't covered here; they get refreshed when
    /// the user switches to them or via their own watcher when one
    /// fires for that tab. The legacy used the same shape — the
    /// active tab is what the user is staring at, the safety net's
    /// job is to keep that tab from going stale silently.
    fn poll_status_safety_net(&mut self) {
        let now = std::time::Instant::now();
        if now.duration_since(self.last_status_refresh).as_secs() >= 30 {
            self.status_dirty = true;
        }
        if !self.status_dirty {
            return;
        }
        let Some(proxy) = self.proxy.clone() else { return };
        if let Some(tab) = self.active_focus_mut() {
            tab.trigger_status_refresh(&proxy);
        }
        // `poll_status_refresh_at` clears status_dirty when the active
        // tab's result lands.
    }

    /// 5 s ref_fingerprint reconciliation on the active tab. Cheap
    /// hash of `git_dir/refs/` content; if it diverges from the last
    /// cached value (something updated the refdb that the watcher
    /// didn't surface) → reopen + trigger a full state refresh.
    /// Belt-and-braces against missed `GitMetadata` events; the
    /// reopen specifically handles libgit2's refdb cache going stale
    /// independently of watcher delivery.
    ///
    /// Skipped when `tab.ref_fingerprint == 0` (no baseline yet — the
    /// initial state refresh hasn't landed) and when a state refresh
    /// is already in flight.
    fn poll_ref_reconciliation(&mut self) {
        let now = std::time::Instant::now();
        if now.duration_since(self.last_ref_check).as_secs() < 5 {
            return;
        }
        self.last_ref_check = now;

        let Some(proxy) = self.proxy.clone() else { return };
        let show_orphans = self.config.show_orphaned_commits;
        let Some(tab) = self.active_focus_mut() else { return };
        if tab.ref_fingerprint == 0 || tab.state_refresh_rx.is_some() {
            return;
        }
        let fresh = crate::git::ref_fingerprint(tab.repo.git_dir());
        if fresh != 0 && fresh != tab.ref_fingerprint {
            tab.reopen_repo_handles();
            tab.trigger_state_refresh(&proxy, show_orphans);
        }
    }

    fn dispatch_watcher_events_at(
        &mut self,
        tab_idx: usize,
        depth: Option<usize>,
        proxy: &winit::event_loop::EventLoopProxy<()>,
        show_orphans: bool,
    ) {
        // Drain + coalesce in one borrow scope so the dispatch below
        // can take its own borrow.
        let max_kind = {
            let Some(tab) = self.tab_at_mut(tab_idx, depth) else { return };
            let Some(rx) = tab.watcher_rx.as_ref() else { return };
            let mut max_kind: Option<crate::watcher::FsChangeKind> = None;
            while let Ok(kind) = rx.try_recv() {
                max_kind = Some(match max_kind {
                    Some(prev) if prev.priority() >= kind.priority() => prev,
                    _ => kind,
                });
            }
            max_kind
        };
        let Some(kind) = max_kind else { return };

        use crate::watcher::FsChangeKind;
        match kind {
            FsChangeKind::WorkingTree => {
                let (tab_id, repo_workdir, worktrees) = {
                    let Some(tab) = self.tab_at_mut(tab_idx, depth) else { return };
                    tab.trigger_status_refresh(proxy);
                    (
                        tab.id,
                        tab.repo.workdir().map(|p| p.to_path_buf()),
                        tab.worktrees.clone(),
                    )
                };
                // Re-check worktree dirty state — a working-tree edit
                // may have flipped a worktree's pill.
                self.dirty_checks_in_flight += crate::git_async::spawn_dirty_checks(
                    tab_id,
                    &[],
                    &worktrees,
                    repo_workdir,
                    &self.dirty_check_tx,
                    proxy,
                );
            }
            FsChangeKind::GitMetadata | FsChangeKind::WorktreeStructure => {
                if let Some(tab) = self.tab_at_mut(tab_idx, depth) {
                    // Bypass libgit2's refdb cache before triggering
                    // the refresh — without this the worker would
                    // still see the pre-event HEAD/refs.
                    tab.reopen_repo_handles();
                    tab.trigger_state_refresh(proxy, show_orphans);
                }
                // WorktreeStructure additionally needs `watcher_paths_changed`
                // handling, which falls out automatically: the state
                // refresh's apply step calls `merge_worktree_views`
                // which sets the flag, and `poll_state_refresh_at`
                // calls `update_worktree_watches`.
            }
        }
    }

    /// Drain finished `StatusResult`s for every tab + drilled-in level.
    fn poll_status_refreshes(&mut self) {
        for tab_idx in 0..self.tabs.len() {
            self.poll_status_refresh_at(tab_idx, None);
            let mut d = 0;
            while let Some(t) = self.tabs.get(tab_idx)
                && d < t.nav_stack.len()
            {
                self.poll_status_refresh_at(tab_idx, Some(d));
                d += 1;
            }
        }
    }

    fn poll_status_refresh_at(&mut self, tab_idx: usize, depth: Option<usize>) {
        let result = {
            let Some(tab) = self.tab_at_mut(tab_idx, depth) else { return };
            let Some(rx) = tab.status_rx.take() else { return };
            match rx.try_recv() {
                Ok(result) => Some(result),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tab.status_rx = Some(rx);
                    None
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
            }
        };
        let Some(result) = result else { return };
        if let Some(tab) = self.tab_at_mut(tab_idx, depth) {
            tab.apply_status_result(result);
        }
        // Stamp last_status_refresh on the active tab's result so the
        // 30 s safety net doesn't redundantly fire.
        if depth.is_none() && tab_idx == self.active_tab {
            self.last_status_refresh = std::time::Instant::now();
            self.status_dirty = false;
        }
    }

    /// Drain the global per-entity dirty-check channel and route each
    /// result back to its originating `RepoTab` by `tab_id`. Stale
    /// results from closed tabs match no live tab and drop silently.
    fn poll_dirty_checks(&mut self) {
        if self.dirty_checks_in_flight == 0 {
            return;
        }
        loop {
            match self.dirty_check_rx.try_recv() {
                Ok(result) => {
                    self.dirty_checks_in_flight = self.dirty_checks_in_flight.saturating_sub(1);
                    let target_id = result.tab_id();
                    if let Some(tab) = self.find_tab_by_id_mut(target_id) {
                        tab.apply_dirty_check_result(result);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // Should never happen — we hold the sender.
                    self.dirty_checks_in_flight = 0;
                    break;
                }
            }
        }
    }

    /// Locate a tab (or drilled-in submodule view) by its stable `id`.
    /// Walks every tab's `nav_stack` so dirty-check results targeting a
    /// drilled-in level land on the right frame.
    fn find_tab_by_id_mut(&mut self, id: u64) -> Option<&mut RepoTab> {
        for tab in &mut self.tabs {
            if tab.id == id {
                return Some(tab);
            }
            for sub in &mut tab.nav_stack {
                if sub.id == id {
                    return Some(sub);
                }
            }
        }
        None
    }

    /// Address a tab + optional drill-in depth as a single mut ref.
    /// `depth: None` is the outer tab; `Some(d)` is `nav_stack[d]`.
    fn tab_at_mut(&mut self, tab_idx: usize, depth: Option<usize>) -> Option<&mut RepoTab> {
        let outer = self.tabs.get_mut(tab_idx)?;
        match depth {
            None => Some(outer),
            Some(d) => outer.nav_stack.get_mut(d),
        }
    }

    /// Fold any completed Gravatar downloads into the in-memory cache
    /// so the next render pass can reach for them via `cache.get(email)`.
    fn drain_avatar_completions(&mut self) {
        if let Some(cache) = self.avatar_cache.as_mut() {
            cache.drain_completions();
        }
    }

    /// Kick off Gravatar fetches for every author we know about.
    /// `AvatarCache::request` is idempotent — repeats short-circuit on
    /// "already requested" — so we just iterate freely each tick.
    /// The cache is created lazily on the first poll where `proxy`
    /// is set, since avatars need an event-loop wake to surface
    /// completions back to the UI.
    fn request_visible_avatars(&mut self) {
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let cache = self
            .avatar_cache
            .get_or_insert_with(|| crate::avatar::AvatarCache::new(proxy));
        for tab in &self.tabs {
            for c in &tab.commits {
                cache.request(&c.author_email);
            }
            for sub in &tab.nav_stack {
                for c in &sub.commits {
                    cache.request(&c.author_email);
                }
            }
        }
    }

    /// Drain any completed diff-stats fetches across every tab + level.
    fn drain_diff_stats(&mut self) {
        for tab in &mut self.tabs {
            tab.drain_diff_stats();
            for sub in &mut tab.nav_stack {
                sub.drain_diff_stats();
            }
        }
    }

    /// Kick off a diff-stats fetch on any tab that doesn't already
    /// have one in flight or completed for its current commit list.
    /// Idempotent — `trigger_diff_stats_fetch` short-circuits when
    /// state says it's already covered.
    fn trigger_diff_stats_fetches(&mut self) {
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        for tab in &mut self.tabs {
            tab.trigger_diff_stats_fetch(proxy.clone());
            for sub in &mut tab.nav_stack {
                sub.trigger_diff_stats_fetch(proxy.clone());
            }
        }
    }

    /// Drain CI fetch receivers for every tab + drilled-in level.
    /// Quiet when nothing's in flight.
    fn drain_ci_receivers(&mut self) {
        for tab in &mut self.tabs {
            tab.drain_ci_receivers();
            for sub in &mut tab.nav_stack {
                sub.drain_ci_receivers();
            }
        }
    }

    /// Per-tab CI refresh on a dynamic interval:
    /// - 15 s when any provider is Pending or a push completed within
    ///   the last 5 minutes (so users see CI light up shortly after
    ///   they push).
    /// - 5 min otherwise.
    ///
    /// Skips tabs with an in-flight fetch to avoid stacking requests
    /// when the network is slow. The first call kicks off immediately
    /// (no `last_ci_fetch` yet).
    fn poll_ci_refresh(&mut self) {
        use std::time::Instant;
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let now = Instant::now();
        // Disjoint borrow: tabs and config are separate fields, so we
        // can hand both into trigger_ci_fetch without `self` at all.
        let config = &mut self.config;
        for outer in &mut self.tabs {
            // Polls one level. Pulled out of the loop body since we
            // visit both the outermost tab and every drilled-in level
            // — each one is a fully-independent CI subject.
            poll_ci_refresh_for(outer, config, &proxy, now);
            for sub in &mut outer.nav_stack {
                poll_ci_refresh_for(sub, config, &proxy, now);
            }
        }
    }

    fn poll_clone_op(&mut self) {
        // Match-and-take: drain the receiver, drop the slot once Ready
        // or Disconnected, then act on the result. On success the
        // dialog auto-closes and the new repo opens as a tab; on
        // failure we surface an Error modal carrying the captured
        // stderr (the modal's body wraps long messages).
        let outcome = match &self.clone_op {
            Some(op) => match op.rx.try_recv() {
                Ok(result) => Some(result),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(
                    "Clone worker disconnected unexpectedly".to_string(),
                )),
            },
            None => None,
        };
        let Some(outcome) = outcome else { return };
        self.clone_op = None;
        match outcome {
            Ok(path) => {
                // Dismiss the Clone modal if it's still open. Users can
                // dismiss it manually mid-clone too — we still open the
                // tab when the op completes.
                if matches!(self.active_modal, Some(ActiveModal::Clone(_))) {
                    self.active_modal = None;
                }
                match RepoTab::open(&path) {
                    Ok(tab) => {
                        self.tabs.push(tab);
                        self.active_tab = self.tabs.len() - 1;
                        self.toasts.push(ToastSpec::success(format!(
                            "Cloned {}",
                            path.display()
                        )));
                    }
                    Err(e) => {
                        self.active_modal = Some(ActiveModal::Error {
                            title: "Clone succeeded but open failed".to_string(),
                            body: format!("{}: {e}", path.display()),
                        });
                    }
                }
            }
            Err(stderr) => {
                self.active_modal = Some(ActiveModal::Error {
                    title: "Clone failed".to_string(),
                    body: stderr,
                });
            }
        }
    }

    /// Poll the async ops on either the outermost tab at `idx`
    /// (`depth = None`) or one specific drilled-in level
    /// (`depth = Some(d)` indexes into that tab's `nav_stack`).
    /// Splitting the resolution this way lets us reuse the body for
    /// every level in the navigation chain without duplicating it.
    fn poll_async_ops_at(&mut self, idx: usize, depth: Option<usize>) {
        // Match-and-take pattern: try_recv each slot, take the slot
        // if Ready/Disconnected so we drop the receiver. We extract
        // the slot by mem::replace before mutating self further.
        for kind in [
            AsyncKind::Fetch,
            AsyncKind::Pull,
            AsyncKind::Push,
            AsyncKind::Mutation,
        ] {
            let Some(tab) = resolve_tab_mut(&mut self.tabs, idx, depth) else {
                return;
            };
            let slot = match kind {
                AsyncKind::Fetch => &mut tab.fetch_op,
                AsyncKind::Pull => &mut tab.pull_op,
                AsyncKind::Push => &mut tab.push_op,
                AsyncKind::Mutation => &mut tab.mutation_op,
            };
            let outcome = match slot {
                Some(op) => match op.rx.try_recv() {
                    Ok(result) => Some(Ok((std::mem::take(&mut op.label), result))),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(())),
                },
                None => None,
            };
            let Some(outcome) = outcome else { continue };
            // Clear slot before triggering refresh so a refresh that
            // happens to inspect the slot sees it empty.
            match kind {
                AsyncKind::Fetch => tab.fetch_op = None,
                AsyncKind::Pull => tab.pull_op = None,
                AsyncKind::Push => tab.push_op = None,
                AsyncKind::Mutation => tab.mutation_op = None,
            }
            tab.request_state_refresh(self.proxy.as_ref(), self.config.show_orphaned_commits);
            match outcome {
                Ok((label, RemoteOpResult { success: true, .. })) => {
                    self.toasts
                        .push(ToastSpec::success(format!("{} {}", kind.past(), label)));
                    // Push success: stamp the time so poll_ci_refresh
                    // boosts to the 15 s cadence, and kick off an
                    // immediate fetch so the new commit's runs surface
                    // as soon as GitHub/GitLab pick them up.
                    if matches!(kind, AsyncKind::Push) {
                        tab.last_push_time = Some(std::time::Instant::now());
                        if let Some(proxy) = self.proxy.clone() {
                            tab.trigger_ci_fetch(&mut self.config, proxy);
                        }
                    }
                }
                Ok((
                    label,
                    RemoteOpResult {
                        success: false,
                        error,
                    },
                )) => {
                    let (summary, retryable) = classify_git_error(kind.name(), &error);
                    let body = if summary.is_empty() {
                        error.clone()
                    } else {
                        format!("{summary}\n\n{error}")
                    };
                    // Rejected pushes get a Force-push offer rather than a
                    // dead-end Error modal. The label was set at op kickoff
                    // as `"<branch> → <remote>"` — split it back so the
                    // retry hits the same ref.
                    if matches!(kind, AsyncKind::Push)
                        && retryable
                        && let Some((branch, remote)) = label.split_once(" \u{2192} ")
                    {
                        self.active_modal = Some(ActiveModal::Confirm {
                            title: "Push rejected".to_string(),
                            body: format!(
                                "{body}\n\n\
                                 Force push will overwrite remote history. \
                                 Only do this if you're certain no one else has based work on \
                                 {branch}."
                            ),
                            ok_label: "Force push".to_string(),
                            destructive: true,
                            action: ConfirmAction::ForcePush {
                                remote: remote.to_string(),
                                branch: branch.to_string(),
                            },
                        });
                    } else {
                        self.active_modal = Some(ActiveModal::Error {
                            title: format!("{} failed", kind.name()),
                            body,
                        });
                    }
                }
                Err(()) => {
                    self.active_modal = Some(ActiveModal::Error {
                        title: format!("{} failed", kind.name()),
                        body: format!(
                            "{} terminated unexpectedly (worker thread disconnected)",
                            kind.name()
                        ),
                    });
                }
            }
        }
        // AI generation lives in its own slot since the result type
        // (Result<AiResponse, String>) doesn't fit the AsyncKind /
        // RemoteOpResult pattern. Drain it after the git ops so the
        // commit-message draft fold-back happens in the same frame.
        self.poll_ai_op_at(idx, depth);
    }

    fn poll_ai_op_at(&mut self, idx: usize, depth: Option<usize>) {
        let (target_path, result) = {
            let Some(tab) = resolve_tab_mut(&mut self.tabs, idx, depth) else {
                return;
            };
            let Some(op) = tab.ai_op.as_mut() else {
                return;
            };
            match op.rx.try_recv() {
                Ok(result) => (std::mem::take(&mut op.target_path), result),
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => (
                    std::mem::take(&mut op.target_path),
                    Err("AI worker thread disconnected".to_string()),
                ),
            }
        };
        let Some(tab) = resolve_tab_mut(&mut self.tabs, idx, depth) else {
            return;
        };
        tab.ai_op = None;
        match result {
            Ok(response) => {
                if let Some(view) = tab.worktree_views.get_mut(&target_path) {
                    view.commit_subject = response.subject;
                    view.commit_body = response.body;
                    self.toasts.push(ToastSpec::success("Generated commit message"));
                } else {
                    self.toasts.push(ToastSpec::warning(
                        "AI result dropped: target worktree no longer present",
                    ));
                }
            }
            Err(msg) => {
                self.toasts
                    .push(ToastSpec::error(format!("AI generate: {msg}")));
            }
        }
    }

    /// Common entry point: bail with a toast if `slot` is occupied or
    /// if there's no proxy / no remotes. Returns the workdir path for
    /// the caller to hand to the async function.
    fn prepare_remote_op(
        &mut self,
        kind: AsyncKind,
        require_remote: bool,
    ) -> Option<(std::path::PathBuf, winit::event_loop::EventLoopProxy<()>)> {
        let proxy = match self.proxy.clone() {
            Some(p) => p,
            None => {
                self.toasts.push(ToastSpec::error(format!(
                    "{} unavailable: event loop proxy missing",
                    kind.name()
                )));
                return None;
            }
        };
        // Operate on the *focused* RepoTab — when the user has drilled
        // into a submodule, fetch / push / pull target that submodule's
        // repo, not the outermost one. The op slot also lives on the
        // focused tab so concurrent parent + child ops don't share a
        // single slot.
        let tab = self.active_focus()?;
        if require_remote && !tab.repo.has_remotes() {
            self.toasts.push(ToastSpec::error(
                "No remotes configured for this repository",
            ));
            return None;
        }
        let busy = match kind {
            AsyncKind::Fetch => tab.fetch_op.is_some(),
            AsyncKind::Pull => tab.pull_op.is_some(),
            AsyncKind::Push => tab.push_op.is_some(),
            AsyncKind::Mutation => tab.mutation_op.is_some(),
        };
        if busy {
            self.toasts.push(ToastSpec::info(format!(
                "{} already in progress",
                kind.name()
            )));
            return None;
        }
        // Run the git CLI in the active worktree's working directory so
        // ops resolve HEAD against that worktree (push picks up the right
        // branch, fetch updates the right remote-tracking refs).
        Some((tab.active_repo().git_command_dir(), proxy))
    }

    fn fetch(&mut self) {
        let Some((wd, proxy)) = self.prepare_remote_op(AsyncKind::Fetch, true) else {
            return;
        };
        let Some(tab) = self.active_focus_mut() else { return };
        let remote = tab
            .repo
            .default_remote()
            .unwrap_or_else(|_| "origin".to_string());
        // Auto-fix missing fetch refspec for bare-cloned remotes — the
        // old whisper-git pattern. Silent on success since this is a
        // common state on fresh `git clone --bare` repos.
        if tab.repo.remote_missing_fetch_refspec(&remote) {
            let _ = tab.repo.add_default_fetch_refspec(&remote);
        }
        let rx = crate::git::fetch_remote_async(wd, remote.clone(), proxy);
        tab.fetch_op = Some(TimedOp::new(rx, format!("from {remote}")));
        self.toasts
            .push(ToastSpec::info(format!("Fetching from {remote}…")));
    }

    fn push(&mut self) {
        let Some((wd, proxy)) = self.prepare_remote_op(AsyncKind::Push, true) else {
            return;
        };
        let Some(tab) = self.active_focus_mut() else { return };
        let remote = tab
            .repo
            .default_remote()
            .unwrap_or_else(|_| "origin".to_string());
        let branch = tab.current_branch().to_string();
        if branch.is_empty() {
            self.toasts
                .push(ToastSpec::error("Push: HEAD is detached, no branch"));
            return;
        }
        let rx = crate::git::push_remote_async(wd, remote.clone(), branch.clone(), proxy);
        let Some(tab) = self.active_focus_mut() else { return };
        tab.push_op = Some(TimedOp::new(rx, format!("{branch} → {remote}")));
        self.toasts
            .push(ToastSpec::info(format!("Pushing {branch} to {remote}…")));
    }

    /// Pull the upstream of the current branch — `git pull <remote> <branch>`,
    /// where `<remote>` is `default_remote()` (the upstream's remote when
    /// tracking info exists; falls back to `origin`) and `<branch>` is
    /// the current branch shorthand. Detached HEAD is rejected up-front
    /// since pull has no source to use.
    fn pull(&mut self) {
        let Some((wd, proxy)) = self.prepare_remote_op(AsyncKind::Pull, true) else {
            return;
        };
        let Some(tab) = self.active_focus_mut() else { return };
        let remote = tab
            .repo
            .default_remote()
            .unwrap_or_else(|_| "origin".to_string());
        let branch = tab.current_branch().to_string();
        if branch.is_empty() {
            self.toasts
                .push(ToastSpec::error("Pull: HEAD is detached, no branch"));
            return;
        }
        let rx = crate::git::pull_remote_async(wd, remote.clone(), branch.clone(), proxy);
        let Some(tab) = self.active_focus_mut() else { return };
        tab.pull_op = Some(TimedOp::new(rx, format!("{remote}/{branch}")));
        self.toasts
            .push(ToastSpec::info(format!("Pulling {remote}/{branch}…")));
    }

    fn cherry_pick(&mut self, oid: git2::Oid) {
        let Some((wd, proxy)) = self.prepare_remote_op(AsyncKind::Mutation, false) else {
            return;
        };
        let sha = oid.to_string();
        let rx = crate::git::cherry_pick_async(wd, sha.clone(), proxy);
        let Some(tab) = self.active_focus_mut() else { return };
        let short = &sha[..7];
        tab.mutation_op = Some(TimedOp::new(rx, format!("cherry-pick {short}")));
        self.toasts
            .push(ToastSpec::info(format!("Cherry-picking {short}…")));
    }

    fn revert(&mut self, oid: git2::Oid) {
        let Some((wd, proxy)) = self.prepare_remote_op(AsyncKind::Mutation, false) else {
            return;
        };
        let sha = oid.to_string();
        let rx = crate::git::revert_commit_async(wd, sha.clone(), proxy);
        let Some(tab) = self.active_focus_mut() else { return };
        let short = &sha[..7];
        tab.mutation_op = Some(TimedOp::new(rx, format!("revert {short}")));
        self.toasts
            .push(ToastSpec::info(format!("Reverting {short}…")));
    }

    /// `git merge <source>` into the current branch. Reached from the
    /// branch context menu in the sidebar — `source` is the picked
    /// branch's name (`"feature/x"` for local, `"origin/main"` for
    /// remote-tracking).
    fn merge_branch(&mut self, source: String) {
        let Some((wd, proxy)) = self.prepare_remote_op(AsyncKind::Mutation, false) else {
            return;
        };
        let rx = crate::git::merge_branch_async(wd, source.clone(), proxy);
        let Some(tab) = self.active_focus_mut() else { return };
        tab.mutation_op = Some(TimedOp::new(rx, format!("merge {source}")));
        self.toasts
            .push(ToastSpec::info(format!("Merging {source}…")));
    }

    fn stash_apply(&mut self, idx: usize) {
        let Some((wd, proxy)) = self.prepare_remote_op(AsyncKind::Mutation, false) else {
            return;
        };
        let rx = crate::git::stash_apply_async(wd, idx, proxy);
        let Some(tab) = self.active_focus_mut() else { return };
        tab.mutation_op = Some(TimedOp::new(rx, format!("stash apply @{{{idx}}}")));
        self.toasts
            .push(ToastSpec::info(format!("Applying stash @{{{idx}}}…")));
    }

    fn stash_pop(&mut self, idx: usize) {
        let Some((wd, proxy)) = self.prepare_remote_op(AsyncKind::Mutation, false) else {
            return;
        };
        let rx = crate::git::stash_pop_index_async(wd, idx, proxy);
        let Some(tab) = self.active_focus_mut() else { return };
        tab.mutation_op = Some(TimedOp::new(rx, format!("stash pop @{{{idx}}}")));
        self.toasts
            .push(ToastSpec::info(format!("Popping stash @{{{idx}}}…")));
    }

    fn stash_drop(&mut self, idx: usize) {
        let Some((wd, proxy)) = self.prepare_remote_op(AsyncKind::Mutation, false) else {
            return;
        };
        let rx = crate::git::stash_drop_async(wd, idx, proxy);
        let Some(tab) = self.active_focus_mut() else { return };
        tab.mutation_op = Some(TimedOp::new(rx, format!("stash drop @{{{idx}}}")));
        self.toasts
            .push(ToastSpec::info(format!("Dropping stash @{{{idx}}}…")));
    }

    /// `git rebase --autostash <base>`. `--autostash` preserves
    /// uncommitted work across the rebase rather than failing on a dirty
    /// tree — matches what experienced users typically want and avoids
    /// "couldn't rebase, fix your tree first" footguns. `base` is the
    /// picked branch (`"main"` for local, `"origin/main"` for
    /// remote-tracking).
    fn rebase_onto(&mut self, base: String) {
        let Some((wd, proxy)) = self.prepare_remote_op(AsyncKind::Mutation, false) else {
            return;
        };
        let rx = crate::git::rebase_with_options_async(wd, base.clone(), true, false, proxy);
        let Some(tab) = self.active_focus_mut() else { return };
        tab.mutation_op = Some(TimedOp::new(rx, format!("rebase onto {base}")));
        self.toasts
            .push(ToastSpec::info(format!("Rebasing onto {base}…")));
    }

    /// Run a sync git op on the active tab. On success, refresh status
    /// + emit a success toast tagged with `label`. On failure, emit an
    /// error toast carrying the underlying message.
    fn run_op<F>(&mut self, label: &str, op: F)
    where
        F: FnOnce(&mut RepoTab) -> anyhow::Result<()>,
    {
        // Operate on the focused tab — when drilled into a submodule,
        // stage / unstage / hunk ops target the submodule's working
        // directory, not the parent's.
        let proxy = self.proxy.clone();
        let show_orphans = self.config.show_orphaned_commits;
        let Some(tab) = self.active_focus_mut() else {
            return;
        };
        match op(tab) {
            Ok(()) => {
                tab.request_state_refresh(proxy.as_ref(), show_orphans);
                self.toasts.push(ToastSpec::success(format!("{label} ✓")));
            }
            Err(e) => {
                self.toasts
                    .push(ToastSpec::error(format!("{label} failed: {e}")));
            }
        }
    }

    fn stage_all(&mut self) {
        self.run_op("Stage all", |t| {
            // Stage each unstaged + untracked file. We could use
            // `git add -A` via CLI for a single batch, but stage_file
            // already handles per-path errors gracefully and keeps us
            // out of process-spawn territory. Collect paths first so
            // we don't hold an immutable borrow across the mutating
            // calls. Both reads and writes go through the active
            // worktree's repo so the staging applies to the right
            // working tree.
            let Some(view) = t.active_view() else { return Ok(()) };
            let paths: Vec<String> = view
                .status
                .unstaged
                .iter()
                .chain(view.status.untracked.iter())
                .map(|f| f.path.clone())
                .collect();
            for p in paths {
                view.repo.stage_file(&p)?;
            }
            Ok(())
        });
    }

    fn unstage_all(&mut self) {
        self.run_op("Unstage all", |t| {
            let Some(view) = t.active_view() else { return Ok(()) };
            let paths: Vec<String> =
                view.status.staged.iter().map(|f| f.path.clone()).collect();
            for p in paths {
                view.repo.unstage_file(&p)?;
            }
            Ok(())
        });
    }

    /// Select the commit `oid` on the focused tab, opening the
    /// commit-detail pane in the right column. Surfaces a toast naming
    /// the ref so the click never feels silent — particularly since
    /// the row may be off-screen until aetna grows a scroll-to-index
    /// API. Quietly no-ops on `None` so callers can pass an unresolved
    /// lookup result without a separate guard.
    fn jump_to_commit(&mut self, oid: Option<git2::Oid>, ref_name: &str) {
        let Some(oid) = oid else {
            self.toasts
                .push(ToastSpec::warning(format!("Couldn't resolve {ref_name}")));
            return;
        };
        let short = oid.to_string()[..7].to_string();
        if let Some(tab) = self.active_focus_mut() {
            tab.select_commit(Some(oid));
            // Clear any sticky diff selection so the right-pane swap
            // (commit detail) actually shows for this jump.
            if let Some(view) = tab.active_view_mut() {
                view.selected_diff_file = None;
            }
        }
        self.toasts
            .push(ToastSpec::info(format!("{ref_name} \u{2192} {short}")));
    }

    fn commit(&mut self) {
        // Operate on the focused tab — when drilled into a submodule,
        // the commit lands in the submodule's working dir, and we
        // detect divergence from the pin afterwards to offer the
        // post-commit coordination dialog.
        let proxy = self.proxy.clone();
        let show_orphans = self.config.show_orphaned_commits;
        let Some(tab) = self.active_focus_mut() else {
            return;
        };
        let pinned_oid = tab.pinned_oid;
        let pinned_path = tab.pinned_path.clone();
        let Some(view) = tab.active_view_mut() else {
            self.toasts
                .push(ToastSpec::warning("No worktree selected"));
            return;
        };
        if view.commit_subject.trim().is_empty() {
            self.toasts
                .push(ToastSpec::warning("Commit subject is empty"));
            return;
        }
        if view.status.staged.is_empty() {
            self.toasts.push(ToastSpec::warning("No staged changes"));
            return;
        }
        let message = if view.commit_body.trim().is_empty() {
            view.commit_subject.clone()
        } else {
            format!(
                "{}\n\n{}",
                view.commit_subject.trim(),
                view.commit_body.trim()
            )
        };
        let new_oid = match view.repo.commit(&message) {
            Ok(oid) => oid,
            Err(e) => {
                self.toasts
                    .push(ToastSpec::error(format!("Commit failed: {e}")));
                return;
            }
        };
        view.commit_subject.clear();
        view.commit_body.clear();
        tab.request_state_refresh(proxy.as_ref(), show_orphans);
        let short = new_oid.to_string()[..7].to_string();
        self.toasts
            .push(ToastSpec::success(format!("Committed {short}")));

        // Post-commit coordination: when this commit landed in a
        // drilled-in submodule and the new HEAD diverges from the
        // parent's pin, offer to stage the parent's pointer update
        // so the user doesn't need to remember to climb back up and
        // `git add <submodule>` themselves.
        if let Some(sm_path) = pinned_path
            && pinned_oid != Some(new_oid)
        {
            let pin_label = pinned_oid
                .map(|o| o.to_string()[..7].to_string())
                .unwrap_or_else(|| "(unset)".to_string());
            self.active_modal = Some(ActiveModal::Confirm {
                title: "Update parent's submodule pointer?".to_string(),
                body: format!(
                    "You committed in submodule '{sm_path}'. The parent currently \
                     pins {pin_label}; your new commit is {short}.\n\n\
                     Choose 'Update pointer' to stage the parent's pointer change \
                     and return to the parent view, where you can review and commit \
                     the update. 'Not now' keeps the existing pin and stays here."
                ),
                ok_label: "Update pointer".to_string(),
                destructive: false,
                action: ConfirmAction::UpdateSubmodulePin { sm_path },
            });
        }
    }
}

fn parse_section(key: &str) -> Option<SidebarSection> {
    SidebarSection::ALL.iter().copied().find(|s| s.key() == key)
}

fn parse_sidebar_target(route: &str) -> Option<ContextTarget> {
    if let Some(name) = route.strip_prefix("branch:") {
        return Some(ContextTarget::LocalBranch(name.to_string()));
    }
    if let Some(rest) = route.strip_prefix("remote:") {
        if let Some((remote, branch)) = rest.split_once('/') {
            return Some(ContextTarget::RemoteBranch {
                remote: remote.to_string(),
                branch: branch.to_string(),
            });
        }
    }
    if let Some(name) = route.strip_prefix("tag:") {
        return Some(ContextTarget::Tag(name.to_string()));
    }
    if let Some(idx_str) = route.strip_prefix("stash:") {
        if let Ok(idx) = idx_str.parse::<usize>() {
            return Some(ContextTarget::Stash(idx));
        }
    }
    None
}

fn sidebar_context_menu(state: &ContextMenuState) -> El {
    use aetna_core::widgets::popover::{context_menu, menu_item};

    let items: Vec<El> = match &state.target {
        ContextTarget::LocalBranch(_) => vec![
            menu_item("Checkout").key("ctx:checkout"),
            menu_item("Merge into HEAD").key("ctx:merge"),
            menu_item("Rebase HEAD onto").key("ctx:rebase"),
            menu_item("Delete").key("ctx:delete"),
        ],
        ContextTarget::RemoteBranch { .. } => vec![
            menu_item("Checkout").key("ctx:checkout"),
            menu_item("Merge into HEAD").key("ctx:merge"),
            menu_item("Rebase HEAD onto").key("ctx:rebase"),
        ],
        ContextTarget::Tag(_) => vec![menu_item("Delete").key("ctx:delete")],
        ContextTarget::Stash(_) => vec![
            menu_item("Apply").key("ctx:apply"),
            menu_item("Pop").key("ctx:pop"),
            menu_item("Drop").key("ctx:drop"),
        ],
        ContextTarget::Commit(_) => vec![
            menu_item("Copy SHA").key("ctx:copy_sha"),
            menu_item("Checkout (detached)").key("ctx:checkout"),
            menu_item("Reset hard to here").key("ctx:reset_hard"),
            menu_item("Cherry-pick").key("ctx:cherry_pick"),
            menu_item("Revert").key("ctx:revert"),
        ],
    };

    context_menu(SIDEBAR_CTX_KEY, state.pos, items)
}

// ---------------------------------------------------------------------------
// Chrome composition
// ---------------------------------------------------------------------------

fn tab_bar(app: &WhisperApp) -> El {
    // Aetna's `editor_tabs` wrapper is the doc-tab strip we want, but
    // it doesn't thread a per-tab leading element. Per aetna's
    // dogfood path (see widget_kit.md and the editor_tab doc), apps
    // that need a leading slot compose the strip themselves with
    // `editor_tab` calls + the trailing `+` add button.
    //
    // Tab values are the indices as strings; routed keys are
    // `tabs:tab:{i}`, `tabs:close:{i}`, `tabs:add`. We keep our own
    // dispatch in `handle_action` rather than delegating to
    // `editor_tabs::apply_event` because (a) closing the last tab
    // should leave whisper-git on the welcome view (not refuse), and
    // (b) `+` opens the rfd file picker asynchronously rather than
    // minting a fresh value synchronously.
    use aetna_core::widgets::button::icon_button;
    use aetna_core::widgets::editor_tabs::{
        EditorTabsConfig, editor_tab, editor_tab_add_key,
    };

    let active = app.active_tab.to_string();
    let config = EditorTabsConfig::default();

    let mut children: Vec<El> = app
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let value = i.to_string();
            let selected = value == active;
            editor_tab(
                "tabs",
                value,
                tab_ci_pip(t),
                t.repo_name.clone(),
                selected,
                config,
            )
        })
        .collect();

    let add_btn = icon_button(IconName::Plus)
        .key(editor_tab_add_key("tabs"))
        .icon_size(tokens::ICON_SM)
        .ghost()
        .width(Size::Fixed(tokens::CONTROL_HEIGHT))
        .height(Size::Fixed(tokens::CONTROL_HEIGHT));
    children.push(add_btn);

    row(children)
        .gap(tokens::SPACE_1)
        .align(Align::Center)
        .padding(Sides::xy(tokens::SPACE_2, tokens::SPACE_1))
        .fill(tokens::MUTED)
        .width(Size::Fill(1.0))
        .height(Size::Hug)
}

/// Aggregate CI status across `tab.ci_results` and render a small
/// colored circle for the tab leading slot. Returns `None` when no
/// provider has reported anything yet — the tab's leading slot stays
/// empty rather than reserving space for a phantom pip.
fn tab_ci_pip(tab: &RepoTab) -> Option<El> {
    use crate::ci::CiState;
    if tab.ci_results.is_empty() {
        return None;
    }
    let overall = tab.ci_results.iter().fold(CiState::None, |worst, r| {
        worst_state(worst, r.status.state)
    });
    let (color, summary_state) = match overall {
        CiState::Failure => (tokens::DESTRUCTIVE, "failing"),
        CiState::Pending => (tokens::WARNING, "running"),
        CiState::Success => (tokens::SUCCESS, "passing"),
        CiState::None => return None,
    };
    let tip = format!("CI {summary_state}");
    Some(
        El::new(aetna_core::tree::Kind::Group)
            .width(Size::Fixed(8.0))
            .height(Size::Fixed(8.0))
            .fill(color)
            .radius(4.0)
            .tooltip(tip),
    )
}

fn worst_state(a: crate::ci::CiState, b: crate::ci::CiState) -> crate::ci::CiState {
    use crate::ci::CiState;
    fn rank(s: CiState) -> u8 {
        match s {
            CiState::Failure => 3,
            CiState::Pending => 2,
            CiState::Success => 1,
            CiState::None => 0,
        }
    }
    if rank(a) >= rank(b) { a } else { b }
}

/// Parent-context strip shown between the breadcrumb and the header
/// bar when drilled into a submodule. Surfaces what the immediate
/// parent's HEAD is at and how it relates to the pin so the user
/// always knows whether the parent has drifted away from this
/// submodule's pinned commit (or vice versa) without leaving focus
/// mode.
///
/// The strip is intentionally text-first — a compressed graphical
/// timeline (per the design doc's "compressed parent timeline") is a
/// future polish; the content here is what users actually need to
/// answer "wait, what does the parent expect?" Click anywhere on the
/// strip pops back to the parent view.
fn parent_context_strip(outer: &RepoTab) -> Option<El> {
    let focus = outer.active_view_tab();
    let parent = outer.parent_of_focus()?;
    let pinned = focus.pinned_oid?;
    let pinned_short = pinned.to_string()[..7].to_string();
    let parent_branch = parent.current_branch().to_string();
    let parent_head = parent.active_view().and_then(|v| v.head_oid);
    let parent_head_short = parent_head
        .map(|o| o.to_string()[..7].to_string())
        .unwrap_or_else(|| "?".to_string());
    let drift = match parent_head {
        Some(head) => head != pinned,
        None => false,
    };

    let parent_label = if parent_branch.is_empty() {
        format!("{} (detached)", parent.repo_name)
    } else {
        format!("{}/{}", parent.repo_name, parent_branch)
    };

    let pin_chip = row([
        text("pin").caption().muted(),
        text(pinned_short).mono().text_color(tokens::INFO),
    ])
    .gap(tokens::SPACE_1)
    .align(Align::Center);

    let parent_head_color = if drift {
        tokens::WARNING
    } else {
        tokens::SUCCESS
    };
    let parent_head_chip = row([
        text("parent HEAD").caption().muted(),
        text(parent_head_short)
            .mono()
            .text_color(parent_head_color),
    ])
    .gap(tokens::SPACE_1)
    .align(Align::Center);

    let mut bar_items: Vec<El> = vec![
        icon(IconName::ChevronLeft).muted(),
        text(parent_label).caption(),
        pin_chip,
        parent_head_chip,
    ];
    if drift {
        bar_items.push(
            badge("drift").muted().text_color(tokens::WARNING),
        );
    }
    bar_items.push(spacer());
    bar_items.push(
        text("Return to parent (Esc)")
            .caption()
            .muted(),
    );

    let depth_to_parent = outer.nav_depth().saturating_sub(1);
    let bar = row(bar_items)
        .gap(tokens::SPACE_2)
        .align(Align::Center)
        .padding(Sides::xy(tokens::SPACE_4, tokens::SPACE_1))
        .key(format!("nav:exit_to:{depth_to_parent}"))
        .focusable()
        .cursor(Cursor::Pointer)
        .tooltip("Return to parent view");
    Some(column([bar, separator()]).width(Size::Fill(1.0)))
}

/// Sibling-submodule strip shown at the bottom of the body when
/// drilled into a submodule. Lists the *immediate parent's* other
/// registered submodules (i.e. siblings of the current view) as
/// click-routed pills so users can shift laterally without climbing
/// back through the breadcrumb. Returns `None` when not drilled in
/// or when there are no actual siblings (a strip with one entry —
/// just the current view — would be pure chrome noise).
///
/// `outer` is the user-opened tab (the one carrying nav_stack);
/// `focus` is the currently rendered RepoTab. The current view's
/// path is excluded from the strip.
fn sibling_submodule_strip(outer: &RepoTab, focus: &RepoTab) -> Option<El> {
    if outer.nav_depth() == 0 {
        return None;
    }
    let parent = outer.parent_of_focus()?;
    let parent_view = parent.active_view()?;
    if parent_view.submodules.len() < 2 {
        return None;
    }

    // Identify the focused submodule by working-dir path so we can
    // exclude it from the sibling list. Each sibling entry's path is
    // relative to the parent's worktree, so build the absolute and
    // compare.
    let focus_workdir = focus.repo.workdir().map(|p| p.to_path_buf());

    let mut chips: Vec<El> = Vec::with_capacity(parent_view.submodules.len());
    for sib in &parent_view.submodules {
        let abs = parent_view.path.join(&sib.path);
        let is_focused = focus_workdir.as_deref() == Some(abs.as_path());
        if is_focused {
            continue;
        }
        let dirty = sib.is_dirty == Some(true);
        let color = if dirty {
            tokens::WARNING
        } else {
            tokens::INFO
        };
        let label = sib
            .path
            .rsplit('/')
            .next()
            .unwrap_or(sib.path.as_str())
            .to_string();
        let chip = row([
            icon(IconName::Folder).muted(),
            text(label).caption().text_color(color),
        ])
        .gap(tokens::SPACE_1)
        .align(Align::Center)
        .padding(Sides::xy(tokens::SPACE_2, tokens::SPACE_1))
        .fill(color.with_alpha(28))
        .stroke(color.with_alpha(96))
        .key(format!("submodule:switch:{}", sib.path))
        .focusable()
        .cursor(Cursor::Pointer)
        .tooltip(format!("Switch to {}", sib.path));
        chips.push(chip);
    }

    if chips.is_empty() {
        return None;
    }

    let mut strip_children: Vec<El> = vec![text("Siblings").caption().muted()];
    strip_children.extend(chips);

    let bar = row(strip_children)
        .gap(tokens::SPACE_2)
        .align(Align::Center)
        .padding(Sides::xy(tokens::SPACE_4, tokens::SPACE_1));
    Some(column([separator(), bar]).width(Size::Fill(1.0)))
}

/// Submodule navigation breadcrumb — `outer › child › grandchild`.
/// Each segment except the last is click-routed under
/// `nav:exit_to:<depth>` so the user can climb back up by clicking
/// any ancestor; the last is the view currently on screen and is
/// rendered without a route. Hidden by the caller when not drilled in.
fn breadcrumb_bar(names: &[String]) -> El {
    let mut children: Vec<El> = Vec::with_capacity(names.len() * 2);
    for (i, name) in names.iter().enumerate() {
        let is_last = i == names.len() - 1;
        let segment = if is_last {
            // Current focus: foreground tint, not focusable.
            text(name.clone()).label()
        } else {
            // Click route pops to depth = i (i = 0 is root).
            text(name.clone())
                .label()
                .text_color(tokens::PRIMARY)
                .key(format!("nav:exit_to:{i}"))
                .focusable()
                .cursor(Cursor::Pointer)
        };
        children.push(segment);
        if !is_last {
            children.push(
                text("\u{203A}".to_string())
                    .caption()
                    .muted(),
            );
        }
    }

    let bar = row(children)
        .gap(tokens::SPACE_2)
        .align(Align::Center)
        .padding(Sides::xy(tokens::SPACE_4, tokens::SPACE_1));
    column([bar, separator()]).width(Size::Fill(1.0))
}

/// An in-flight async op surfaces in the header as `[spinner] verb · 12s`.
/// After this many seconds the spinner switches to the warning color and
/// gains a "(still running)" suffix — a soft hint that the user may want
/// to investigate before assuming the op will finish.
const STALL_WARN_SECS: u64 = 60;

fn header_bar(active: Option<&RepoTab>, clone_op: Option<&CloneOp>) -> El {
    let branch = match active {
        Some(t) => {
            let cb = t.current_branch();
            let label = if cb.is_empty() {
                "(detached)".to_string()
            } else {
                cb.to_string()
            };
            row([icon(IconName::GitBranch), text(label).label()])
                .gap(tokens::SPACE_1)
                .align(Align::Center)
        }
        None => row([icon(IconName::GitBranch), text("(no repo)").muted()])
            .gap(tokens::SPACE_1)
            .align(Align::Center),
    };

    let actions_enabled = active.is_some();
    let fetch_busy = active.map(|t| t.fetch_op.is_some()).unwrap_or(false);
    let pull_busy = active.map(|t| t.pull_op.is_some()).unwrap_or(false);
    let push_busy = active.map(|t| t.push_op.is_some()).unwrap_or(false);

    let mut fetch_btn = button_with_icon(IconName::Download, "Fetch")
        .key("fetch")
        .tooltip("git fetch");
    if fetch_busy {
        fetch_btn = fetch_btn.disabled();
    }
    let mut pull_btn = button_with_icon(IconName::Download, "Pull")
        .key("pull")
        .tooltip("git pull");
    if pull_busy {
        pull_btn = pull_btn.disabled();
    }
    // Tiny chevron beside Pull opens the Pull picker (non-tracking
    // source + --rebase toggle). Keeping it as its own icon_button
    // preserves the bare Pull-button default for the common case.
    let mut pull_options_btn = icon_button(IconName::ChevronDown)
        .key("pull_options")
        .tooltip("Pull from\u{2026}");
    if pull_busy {
        pull_options_btn = pull_options_btn.disabled();
    }
    let mut push_btn = button_with_icon(IconName::Upload, "Push")
        .key("push")
        .tooltip("git push");
    if push_busy {
        push_btn = push_btn.disabled();
    }
    // Tiny chevron beside Push opens the Push picker (remote/branch
    // override + --force-with-lease / --set-upstream / --tags). Keeps the
    // bare Push-button default for the common case.
    let mut push_options_btn = icon_button(IconName::ChevronDown)
        .key("push_options")
        .tooltip("Push with options\u{2026}");
    if push_busy {
        push_options_btn = push_options_btn.disabled();
    }

    let mut bar_items: Vec<El> = vec![branch];
    if let Some(tab) = active {
        bar_items.extend(ci_badges(tab));
    }
    let status_lines = op_status_lines(active, clone_op);
    if !status_lines.is_empty() {
        bar_items.push(
            column(status_lines)
                .gap(tokens::SPACE_1)
                .align(Align::Center),
        );
    }
    bar_items.push(spacer());
    bar_items.push(toolbar_group([
        fetch_btn,
        pull_btn,
        pull_options_btn,
        push_btn,
        push_options_btn,
        button_with_icon(IconName::GitCommit, "Commit")
            .key("commit")
            .primary()
            .tooltip("Stage and commit (Ctrl+Enter)"),
        icon_button(IconName::Settings)
            .key("settings")
            .tooltip("Settings"),
    ]));

    let bar = toolbar(bar_items)
        .padding(Sides::xy(tokens::SPACE_4, tokens::SPACE_2))
        .opacity(if actions_enabled { 1.0 } else { 0.85 });

    // Toolbar is app chrome, not a boxed content object — the README's
    // smells list flags `card([card_content([toolbar(...)])])` for app
    // headers explicitly. A trailing `separator()` carries the visual
    // break between header and body without the false card silhouette.
    column([bar, separator()]).width(Size::Fill(1.0))
}

/// Build one inline status row per in-flight op for the active tab plus
/// the app-scoped clone, if any. Each row is `[spinner, "Verb label · Ns"]`
/// with a warning treatment after `STALL_WARN_SECS`.
fn op_status_lines(active: Option<&RepoTab>, clone_op: Option<&CloneOp>) -> Vec<El> {
    let mut lines: Vec<El> = Vec::new();
    if let Some(tab) = active {
        if let Some(op) = &tab.fetch_op {
            lines.push(status_row("Fetch", &op.label, op.started.elapsed().as_secs()));
        }
        if let Some(op) = &tab.pull_op {
            lines.push(status_row("Pull", &op.label, op.started.elapsed().as_secs()));
        }
        if let Some(op) = &tab.push_op {
            lines.push(status_row("Push", &op.label, op.started.elapsed().as_secs()));
        }
        if let Some(op) = &tab.mutation_op {
            // mutation labels already carry their own verb ("cherry-pick abc1234"),
            // so don't prefix.
            lines.push(status_row("", &op.label, op.started.elapsed().as_secs()));
        }
    }
    if let Some(op) = clone_op {
        lines.push(status_row(
            "Clone",
            &op.dest_label,
            op.started.elapsed().as_secs(),
        ));
    }
    lines
}

fn status_row(verb: &str, label: &str, secs: u64) -> El {
    use aetna_core::widgets::spinner::spinner_with_color;
    let stalled = secs >= STALL_WARN_SECS;
    let arc_color = if stalled {
        tokens::DESTRUCTIVE
    } else {
        tokens::PRIMARY
    };
    let suffix = if stalled { " (still running)" } else { "" };
    let text_str = if verb.is_empty() {
        format!("{label} \u{00b7} {secs}s{suffix}")
    } else {
        format!("{verb} {label} \u{00b7} {secs}s{suffix}")
    };
    let label_el = if stalled {
        text(text_str).caption().text_color(tokens::DESTRUCTIVE)
    } else {
        text(text_str).caption().muted()
    };
    row([spinner_with_color(arc_color), label_el])
        .gap(tokens::SPACE_2)
        .align(Align::Center)
}

/// Per-provider CI badges for the header bar. One row per provider in
/// `tab.ci_results` — brand mark + state icon, tinted by state, with
/// the human-readable summary as tooltip and a click route that opens
/// the provider URL via `xdg-open`.
fn ci_badges(tab: &RepoTab) -> Vec<El> {
    use crate::ci::CiState;
    use crate::widgets::brand_icons;
    if tab.ci_results.is_empty() {
        return Vec::new();
    }
    tab.ci_results
        .iter()
        .enumerate()
        .map(|(idx, result)| {
            let (state_icon, color) = match result.status.state {
                CiState::Success => (Some(IconName::Check), tokens::SUCCESS),
                CiState::Failure => (Some(IconName::AlertCircle), tokens::DESTRUCTIVE),
                CiState::Pending => (Some(IconName::Activity), tokens::WARNING),
                CiState::None => (None, tokens::MUTED_FOREGROUND),
            };
            let mut children: Vec<El> = vec![
                icon(brand_icons::for_provider(result.provider))
                    .icon_size(14.0)
                    .text_color(color),
            ];
            if let Some(name) = state_icon {
                children.push(icon(name).icon_size(14.0).text_color(color));
            }
            if let Some(counts) = result.status.counts {
                let total = counts.total();
                if total > 0 {
                    let label = match result.status.state {
                        CiState::Failure => format!("{} fail", counts.failure),
                        CiState::Pending => format!("{} run", counts.pending),
                        CiState::Success => format!("{}/{}", counts.success, total),
                        CiState::None => String::new(),
                    };
                    if !label.is_empty() {
                        children.push(text(label).caption().text_color(color));
                    }
                }
            }
            let mut badge = row(children)
                .gap(tokens::SPACE_1)
                .align(Align::Center)
                .padding(Sides::xy(tokens::SPACE_2, tokens::SPACE_1))
                .fill(color.with_alpha(28))
                .stroke(color.with_alpha(96));
            if result.status.url.is_some() {
                badge = badge
                    .key(format!("ci:open:{idx}"))
                    .focusable();
            }
            badge.tooltip(format!(
                "{} \u{00b7} {}",
                result.provider.short_label(),
                result.status.summary
            ))
        })
        .collect()
}

fn shortcut_bar() -> El {
    row([
        kbd("Ctrl+O", "Open"),
        kbd("Ctrl+W", "Close tab"),
        kbd("Ctrl+/", "Toggle shortcuts"),
    ])
    .padding(Sides::xy(tokens::SPACE_4, tokens::SPACE_1))
    .gap(tokens::SPACE_4)
    .align(Align::Center)
}

fn kbd(chord: &str, label: &str) -> El {
    row([badge(chord).muted(), text(label).caption()])
        .gap(tokens::SPACE_1)
        .align(Align::Center)
}

/// Center-pane placeholder shown when the repo has no working tree to
/// operate on (effectively bare with zero linked worktrees). The
/// staging well + diff viewer have nothing to display until the user
/// adds a worktree, so we surface that explicitly rather than rendering
/// empty panes.
fn no_worktree_placeholder() -> El {
    column([
        h3("No worktree selected"),
        paragraph(
            "This repository has no working tree available. Add a linked \
             worktree (`git worktree add`) and select it in the sidebar \
             to start staging changes here.",
        )
        .muted()
        .text_align(TextAlign::Center),
    ])
    .gap(tokens::SPACE_2)
    .align(Align::Center)
    .justify(Justify::Center)
    .padding(tokens::SPACE_4)
    .height(Size::Fill(1.0))
    .width(Size::Fill(1.0))
}

