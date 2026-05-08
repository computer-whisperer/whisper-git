//! Phase 3 App impl: chrome + branch sidebar wired to a real GitRepo.
//!
//! Per-tab data lives in `RepoTab` (see `repo_tab.rs`); the sidebar
//! composer lives in `sidebar.rs`. Staging / diff / graph still
//! placeholders in the main area.

use std::path::Path;

use aetna_core::{
    App, AppShader, BuildCx, El, IconName, KeyChord, KeyModifiers, Selection, Theme, UiEvent,
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
use crate::dialogs::{CloneForm, TokenForm};
use crate::repo_tab::{RepoTab, RepoView, SidebarSection, TimedOp};
use crate::token_store;
use crate::sidebar;
use crate::staging;
use crate::welcome;

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
    Worktree(String),
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
}

/// commit_node.wgsl — copied verbatim from the aetna `custom_paint`
/// example. Per-row commit-graph cell: vertical lane line + circle
/// node. Registered up front so Phase 6 doesn't need to retrofit
/// shader wiring into the host.
pub const COMMIT_NODE_WGSL: &str = r#"
struct FrameUniforms { viewport: vec2<f32>, _pad: vec2<f32>, };
@group(0) @binding(0) var<uniform> frame: FrameUniforms;

struct VertexInput  { @location(0) corner_uv: vec2<f32>, };
struct InstanceInput {
    @location(1) rect:  vec4<f32>,
    @location(2) vec_a: vec4<f32>,
    @location(3) vec_b: vec4<f32>,
    @location(4) vec_c: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) @interpolate(perspective, sample) local_px: vec2<f32>,
    @location(1) size:   vec2<f32>,
    @location(2) fill:   vec4<f32>,
    @location(3) ring:   vec4<f32>,
    @location(4) params: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput, inst: InstanceInput) -> VertexOutput {
    let pos_px = in.corner_uv * inst.rect.zw + inst.rect.xy;
    let clip = vec4<f32>(
        pos_px.x / frame.viewport.x * 2.0 - 1.0,
        1.0 - pos_px.y / frame.viewport.y * 2.0,
        0.0, 1.0,
    );
    var out: VertexOutput;
    out.clip_pos = clip;
    out.local_px = in.corner_uv * inst.rect.zw;
    out.size     = inst.rect.zw;
    out.fill     = inst.vec_a;
    out.ring     = inst.vec_b;
    out.params   = inst.vec_c;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let radius = in.params.x;
    let ring_w = in.params.y;
    let line_w = in.params.z;
    let lane_x = in.params.w * in.size.x;
    let row_y  = in.size.y * 0.5;

    let p   = in.local_px - vec2<f32>(lane_x, row_y);
    let d   = length(p) - radius;
    let aa  = max(fwidth(d), 0.5);
    let outer = 1.0 - smoothstep(0.0, aa, d);
    let inner = 1.0 - smoothstep(0.0, aa, d + ring_w);
    let ring_a = clamp(outer - inner, 0.0, 1.0);
    let body_a = inner;

    let dx     = abs(in.local_px.x - lane_x);
    let aa_l   = max(fwidth(dx), 0.5);
    let line_a = (1.0 - smoothstep(line_w * 0.5 - aa_l,
                                    line_w * 0.5 + aa_l, dx))
                 * (1.0 - outer);

    let line_pm = vec4<f32>(in.ring.rgb * (in.ring.a * line_a), in.ring.a * line_a);
    let ring_pm = vec4<f32>(in.ring.rgb * (in.ring.a * ring_a), in.ring.a * ring_a);
    let body_pm = vec4<f32>(in.fill.rgb * (in.fill.a * body_a), in.fill.a * body_a);
    let pm = line_pm + ring_pm + body_pm;
    let a  = clamp(pm.a, 0.0, 1.0);
    if (a <= 0.0) { return vec4<f32>(0.0); }
    return vec4<f32>(pm.rgb / a, a);
}
"#;

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
        }
    }

    /// Construct with already-built tabs. Used by `dump_bundles` which
    /// fabricates synthetic repos. Config is `Default::default()` so
    /// dumped scenes are hermetic across developer machines.
    pub fn with_tabs(tabs: Vec<RepoTab>) -> Self {
        let config = Config::default();
        let sidebar_w = config.sidebar_w;
        let right_pane_w = config.right_pane_w;
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
        }
    }

    fn active(&self) -> Option<&RepoTab> {
        self.tabs.get(self.active_tab)
    }

    fn active_mut(&mut self) -> Option<&mut RepoTab> {
        self.tabs.get_mut(self.active_tab)
    }
}

impl App for WhisperApp {
    fn before_build(&mut self) {
        self.poll_async_ops();
    }

    fn build(&self, _cx: &BuildCx) -> El {
        let mut chrome: Vec<El> = Vec::with_capacity(3);
        if !self.tabs.is_empty() {
            chrome.push(tab_bar(self));
        }
        chrome.push(header_bar(self.active(), self.clone_op.as_ref()));
        if self.shortcut_bar_visible {
            chrome.push(shortcut_bar());
        }
        let chrome_el = column(chrome).gap(0.0);

        let body = match self.active() {
            Some(tab) => {
                let has_right_pane = !matches!(
                    (tab.view_mode, tab.active_view()),
                    (RepoView::Working, None)
                );
                let (center, right) = match tab.view_mode {
                    RepoView::Working => match tab.active_view() {
                        Some(view) => {
                            // The selector + staging-well pair share the
                            // user-resized right-pane width; override the
                            // default `STAGING_WIDTH` Fixed at the outer
                            // wrapper so both stay aligned.
                            let staging_pane = match staging::worktree_selector(tab) {
                                Some(sel) => column([
                                    sel,
                                    staging::staging_well(view, &self.selection),
                                ])
                                .gap(0.0)
                                .height(Size::Fill(1.0)),
                                None => staging::staging_well(view, &self.selection),
                            };
                            (diff_view::diff_view(view), staging_pane)
                        }
                        None => (no_worktree_placeholder(), spacer().width(Size::Fixed(0.0))),
                    },
                    RepoView::History => (
                        commit_graph::history_view(tab),
                        commit_details::commit_details_pane(tab),
                    ),
                };

                // Resizable layout: sidebar | resize_handle | center
                // (| resize_handle | right). The handle widgets live as
                // siblings inside the row and route drag events to the
                // app via their keys; aetna's `apply_event_fixed` folds
                // the drag delta back into the size value.
                let mut children: Vec<El> = vec![
                    sidebar::sidebar(tab).width(Size::Fixed(self.sidebar_w)),
                    resize_handle(Axis::Row).key("sidebar:resize"),
                    center,
                ];
                if has_right_pane {
                    children.push(resize_handle(Axis::Row).key("right:resize"));
                    children.push(right.width(Size::Fixed(self.right_pane_w)));
                }
                row(children).gap(0.0).height(Size::Fill(1.0))
            }
            None => welcome::welcome_view(&self.config.recent_repos),
        };

        let main = column([chrome_el, body]).gap(0.0);
        let modal_layer = self.active_modal.as_ref().map(|m| match m {
            ActiveModal::Settings => {
                dialogs::settings_modal(&self.config, self.shortcut_bar_visible)
            }
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
                dialogs::token_modal(form, &self.selection, github_set)
            }
        });
        let menu_layer = self
            .context_menu
            .as_ref()
            .map(|cm| sidebar_context_menu(cm));
        overlays(main, [menu_layer, modal_layer])
    }

    fn on_event(&mut self, event: UiEvent) {
        // Escape closes whichever modal is open. Aetna emits an Escape
        // event when the key is pressed and no widget consumes it; our
        // text inputs don't consume Escape, so it always reaches us.
        if matches!(event.kind, UiEventKind::Escape) && self.active_modal.is_some() {
            self.active_modal = None;
            return;
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
            (
                KeyChord::named(UiKey::Enter).with_modifiers(KM_CTRL),
                "commit".to_string(),
            ),
        ]
    }

    fn drain_toasts(&mut self) -> Vec<ToastSpec> {
        std::mem::take(&mut self.toasts)
    }

    fn shaders(&self) -> Vec<AppShader> {
        vec![AppShader {
            name: "commit_node",
            wgsl: COMMIT_NODE_WGSL,
            samples_backdrop: false,
            samples_time: false,
        }]
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

        // tab:{idx}, tab_close:{idx}
        if let Some(idx_str) = key.strip_prefix("tab_close:") {
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

        // section:LOCAL etc.
        if let Some(section_key) = key.strip_prefix("section:") {
            if let Some(section) = parse_section(section_key)
                && let Some(tab) = self.active_mut()
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
        if let Some(path) = key.strip_prefix("diff:") {
            if let Some(view) = self.active_mut().and_then(|t| t.active_view_mut()) {
                view.selected_diff_file = Some(path.to_string());
            }
            return;
        }
        // wt_select:{path} — switch the active worktree. The path is
        // routed verbatim (worktree names aren't always unique across
        // nested checkouts).
        if let Some(path) = key.strip_prefix("wt_select:") {
            if let Some(tab) = self.active_mut() {
                tab.select_worktree(std::path::PathBuf::from(path));
            }
            return;
        }
        // commit:{idx} — selects a commit in the History view.
        if let Some(idx_str) = key.strip_prefix("commit:") {
            if let Ok(idx) = idx_str.parse::<usize>()
                && let Some(tab) = self.active_mut()
            {
                let oid = tab.commits.get(idx).map(|c| c.id);
                tab.select_commit(oid);
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

        // Sidebar worktree click — promote it to active. Sibling sections
        // still emit placeholder toasts (Phase 4c wires those).
        if let Some(name) = key.strip_prefix("worktree:") {
            if let Some(tab) = self.active_mut() {
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

        // Sidebar item clicks — Phase 3 just announces them.
        for prefix in [
            "branch:",
            "remote:",
            "tag:",
            "submodule:",
            "stash:",
        ] {
            if let Some(name) = key.strip_prefix(prefix) {
                let label = prefix.trim_end_matches(':');
                self.toasts.push(ToastSpec::info(format!(
                    "{label}: {name} (Phase 4c wiring)"
                )));
                return;
            }
        }

        match key {
            "open_repo" => self.open_repo_dialog(),
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
            "view:tab:working" => {
                if let Some(tab) = self.active_mut() {
                    tab.view_mode = RepoView::Working;
                }
            }
            "view:tab:history" => {
                if let Some(tab) = self.active_mut() {
                    tab.view_mode = RepoView::History;
                }
            }
            "details:copy_sha" => {
                if let Some(oid) = self.active().and_then(|t| t.selected_commit) {
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
            let Some(oid) = self.active().and_then(|t| t.commits.get(idx).map(|c| c.id)) else {
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

        if key.starts_with("clone:") {
            self.handle_clone_route(key);
            return true;
        }
        if key.starts_with("token:") {
            self.handle_token_route(key);
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
            _ => false,
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
            _ => {}
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
            ("drop", ContextTarget::Stash(idx)) => {
                self.active_modal = Some(ActiveModal::Confirm {
                    title: "Drop stash".to_string(),
                    body: format!("Drop stash @{{{idx}}} permanently?"),
                    ok_label: "Drop".to_string(),
                    destructive: true,
                    action: ConfirmAction::DropStash(idx),
                });
            }
            ("switch", ContextTarget::Worktree(name)) => {
                self.toasts.push(ToastSpec::info(format!(
                    "Switch to worktree '{name}' (Phase 5c)"
                )));
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
            ConfirmAction::DropStash(_idx) => {
                // GitRepo doesn't expose stash_drop sync today; emit a
                // placeholder until Phase 4d brings the rest of the
                // stash op surface online.
                self.toasts.push(ToastSpec::info("Drop stash (Phase 4d)"));
            }
            ConfirmAction::ResetHard(oid) => {
                self.run_op("Reset hard", move |t| {
                    t.repo.reset_to_commit(oid, git2::ResetType::Hard)
                });
            }
            ConfirmAction::ForcePush { remote, branch } => {
                self.force_push(remote, branch);
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
        let Some(tab) = self.active_mut() else { return };
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
    /// called once per frame from `before_build`. Visits every tab so
    /// background work in a non-foreground tab still completes cleanly.
    pub fn poll_async_ops(&mut self) {
        for idx in 0..self.tabs.len() {
            self.poll_async_ops_for_tab(idx);
        }
        self.poll_clone_op();
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

    fn poll_async_ops_for_tab(&mut self, idx: usize) {
        // Match-and-take pattern: try_recv each slot, take the slot
        // if Ready/Disconnected so we drop the receiver. We extract
        // the slot by mem::replace before mutating self further.
        for kind in [
            AsyncKind::Fetch,
            AsyncKind::Pull,
            AsyncKind::Push,
            AsyncKind::Mutation,
        ] {
            let Some(tab) = self.tabs.get_mut(idx) else {
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
            // Clear slot before calling refresh so a refresh that
            // happens to inspect the slot sees it empty.
            match kind {
                AsyncKind::Fetch => tab.fetch_op = None,
                AsyncKind::Pull => tab.pull_op = None,
                AsyncKind::Push => tab.push_op = None,
                AsyncKind::Mutation => tab.mutation_op = None,
            }
            tab.refresh();
            match outcome {
                Ok((label, RemoteOpResult { success: true, .. })) => {
                    self.toasts
                        .push(ToastSpec::success(format!("{} {}", kind.past(), label)));
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
        let active_idx = self.active_tab;
        let tab = match self.tabs.get(active_idx) {
            Some(t) => t,
            None => return None,
        };
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
        let Some(tab) = self.active_mut() else { return };
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
        let Some(tab) = self.active_mut() else { return };
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
        let Some(tab) = self.active_mut() else { return };
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
        let Some(tab) = self.active_mut() else { return };
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
        let Some(tab) = self.active_mut() else { return };
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
        let Some(tab) = self.active_mut() else { return };
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
        let Some(tab) = self.active_mut() else { return };
        let short = &sha[..7];
        tab.mutation_op = Some(TimedOp::new(rx, format!("revert {short}")));
        self.toasts
            .push(ToastSpec::info(format!("Reverting {short}…")));
    }

    /// Run a sync git op on the active tab. On success, refresh status
    /// + emit a success toast tagged with `label`. On failure, emit an
    /// error toast carrying the underlying message.
    fn run_op<F>(&mut self, label: &str, op: F)
    where
        F: FnOnce(&mut RepoTab) -> anyhow::Result<()>,
    {
        let active_idx = self.active_tab;
        let Some(tab) = self.tabs.get_mut(active_idx) else {
            return;
        };
        match op(tab) {
            Ok(()) => {
                tab.refresh();
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

    fn commit(&mut self) {
        let active_idx = self.active_tab;
        let Some(tab) = self.tabs.get_mut(active_idx) else {
            return;
        };
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
        match view.repo.commit(&message) {
            Ok(oid) => {
                view.commit_subject.clear();
                view.commit_body.clear();
                tab.refresh();
                let short = oid.to_string()[..7].to_string();
                self.toasts
                    .push(ToastSpec::success(format!("Committed {short}")));
            }
            Err(e) => {
                self.toasts
                    .push(ToastSpec::error(format!("Commit failed: {e}")));
            }
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
    if let Some(name) = route.strip_prefix("worktree:") {
        return Some(ContextTarget::Worktree(name.to_string()));
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
            menu_item("Delete").key("ctx:delete"),
        ],
        ContextTarget::RemoteBranch { .. } => vec![menu_item("Checkout").key("ctx:checkout")],
        ContextTarget::Tag(_) => vec![menu_item("Delete").key("ctx:delete")],
        ContextTarget::Worktree(_) => vec![menu_item("Switch to worktree").key("ctx:switch")],
        ContextTarget::Stash(_) => vec![menu_item("Drop").key("ctx:drop")],
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
    use aetna_core::widgets::tabs::tab_trigger;

    // tabs_list doesn't accept per-option children, so we compose tabs
    // by hand here (each chip carries a Close button next to its
    // tab_trigger). The outer row keeps the same MUTED-track recipe
    // tabs_list uses internally.
    let mut tabs: Vec<El> = Vec::new();
    for (i, t) in app.tabs.iter().enumerate() {
        let trigger = tab_trigger("tabs", i, t.repo_name.clone(), i == app.active_tab);
        let close = icon_button(IconName::X)
            .key(format!("tab_close:{i}"))
            .tooltip("Close tab");
        tabs.push(
            row([trigger, close])
                .gap(tokens::SPACE_1)
                .align(Align::Center),
        );
    }
    tabs.push(
        icon_button(IconName::Plus)
            .key("open_repo")
            .tooltip("Open repository (Ctrl+O)"),
    );
    row(tabs)
        .fill(tokens::MUTED)
        .stroke(tokens::BORDER)
        .padding(Sides::xy(tokens::SPACE_2, tokens::SPACE_1))
        .gap(tokens::SPACE_1)
        .align(Align::Center)
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
    let view_mode = active.map(|t| t.view_mode).unwrap_or(RepoView::Working);

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
    let mut push_btn = button_with_icon(IconName::Upload, "Push")
        .key("push")
        .tooltip("git push");
    if push_busy {
        push_btn = push_btn.disabled();
    }

    let mut bar_items: Vec<El> = vec![branch, view_mode_segment(view_mode)];
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
        push_btn,
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

    card([card_content([bar])]).width(Size::Fill(1.0))
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

/// Two-button segmented control in the header that toggles the
/// center pane between the working diff and the commit history.
/// Routed keys: `view:tab:working` / `view:tab:history`.
fn view_mode_segment(current: RepoView) -> El {
    use aetna_core::widgets::tabs::tabs_list;
    let current_str = match current {
        RepoView::Working => "working",
        RepoView::History => "history",
    };
    tabs_list(
        "view",
        &current_str,
        [("working", "Working"), ("history", "History")],
    )
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
        h2("No worktree selected"),
        paragraph(
            "This repository has no working tree available. Add a linked \
             worktree (`git worktree add`) and select it in the sidebar \
             to start staging changes here.",
        ),
    ])
    .padding(tokens::SPACE_4)
    .gap(tokens::SPACE_3)
    .height(Size::Fill(1.0))
    .width(Size::Fill(1.0))
}

