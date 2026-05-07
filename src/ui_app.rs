//! Phase 3 App impl: chrome + branch sidebar wired to a real GitRepo.
//!
//! Per-tab data lives in `RepoTab` (see `repo_tab.rs`); the sidebar
//! composer lives in `sidebar.rs`. Staging / diff / graph still
//! placeholders in the main area.

use std::path::Path;

use aetna_core::{
    App, AppShader, BuildCx, El, IconName, KeyChord, KeyModifiers, Selection, UiEvent,
    UiEventKind, UiKey,
    prelude::*,
    toast::ToastSpec,
    widgets::{text_area, text_input},
};

const KM_CTRL: KeyModifiers = KeyModifiers {
    shift: false,
    ctrl: true,
    alt: false,
    logo: false,
};

use crate::config::Config;
use crate::dialogs;
use crate::diff_view;
use crate::repo_tab::{RepoTab, SidebarSection};
use crate::sidebar;
use crate::staging;

/// Pending action that gates a Confirm modal. Carried through `on_event`
/// from the originating action to the OK button. Phase 5b adds branch /
/// stash deletion variants.
#[derive(Clone, Debug)]
pub enum ConfirmAction {
    CloseTab(usize),
}

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
        Self {
            tabs,
            active_tab: 0,
            shortcut_bar_visible: config.shortcut_bar_visible,
            toasts: Vec::new(),
            selection: Selection::default(),
            config,
            active_modal: None,
        }
    }

    /// Construct with already-built tabs. Used by `dump_bundles` which
    /// fabricates synthetic repos. Config is `Default::default()` so
    /// dumped scenes are hermetic across developer machines.
    pub fn with_tabs(tabs: Vec<RepoTab>) -> Self {
        Self {
            tabs,
            active_tab: 0,
            shortcut_bar_visible: true,
            toasts: Vec::new(),
            selection: Selection::default(),
            config: Config::default(),
            active_modal: None,
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
    fn build(&self, _cx: &BuildCx) -> El {
        let mut chrome: Vec<El> = Vec::with_capacity(3);
        if !self.tabs.is_empty() {
            chrome.push(tab_bar(self));
        }
        chrome.push(header_bar(self.active()));
        if self.shortcut_bar_visible {
            chrome.push(shortcut_bar());
        }
        let chrome_el = column(chrome).gap(0.0);

        let body = match self.active() {
            Some(tab) => row([
                sidebar::sidebar(tab),
                diff_view::diff_view(tab),
                staging::staging_well(tab, &self.selection),
            ])
            .gap(0.0)
            .height(Size::Fill(1.0)),
            None => main_placeholder(None),
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
        });
        overlays(main, [modal_layer])
    }

    fn on_event(&mut self, event: UiEvent) {
        // Escape closes whichever modal is open. Aetna emits an Escape
        // event when the key is pressed and no widget consumes it; our
        // text inputs don't consume Escape, so it always reaches us.
        if matches!(event.kind, UiEventKind::Escape) && self.active_modal.is_some() {
            self.active_modal = None;
            return;
        }

        // Text-editing routes consume the event for the active tab's
        // commit-message fields. Index-based borrow so we can hand
        // `self.selection` to the apply_event helpers without conflict.
        let active_idx = self.active_tab;
        if let Some(tab) = self.tabs.get_mut(active_idx) {
            text_input::apply_event(
                &mut tab.commit_subject,
                &mut self.selection,
                "subject",
                &event,
            );
            text_area::apply_event(&mut tab.commit_body, &mut self.selection, "body", &event);
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
        }]
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
        if let Some(idx_str) = key.strip_prefix("tab:") {
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
            self.run_op("Stage", |t| t.repo.stage_file(path));
            return;
        }
        if let Some(path) = key.strip_prefix("unstage_file:") {
            self.run_op("Unstage", |t| t.repo.unstage_file(path));
            return;
        }
        if let Some(path) = key.strip_prefix("diff:") {
            if let Some(tab) = self.active_mut() {
                tab.selected_diff_file = Some(path.to_string());
            }
            return;
        }
        if let Some(rest) = key.strip_prefix("stage_hunk:") {
            if let Some((idx_str, path)) = rest.split_once(':')
                && let Ok(idx) = idx_str.parse::<usize>()
            {
                let path = path.to_string();
                self.run_op("Stage hunk", move |t| t.repo.stage_hunk(&path, idx));
            }
            return;
        }
        if let Some(rest) = key.strip_prefix("unstage_hunk:") {
            if let Some((idx_str, path)) = rest.split_once(':')
                && let Ok(idx) = idx_str.parse::<usize>()
            {
                let path = path.to_string();
                self.run_op("Unstage hunk", move |t| t.repo.unstage_hunk(&path, idx));
            }
            return;
        }

        // Sidebar item clicks — Phase 3 just announces them.
        for prefix in ["branch:", "remote:", "tag:", "submodule:", "worktree:", "stash:"] {
            if let Some(name) = key.strip_prefix(prefix) {
                let label = prefix.trim_end_matches(':');
                self.toasts
                    .push(ToastSpec::info(format!("{label}: {name} (Phase 4c wiring)")));
                return;
            }
        }

        match key {
            "open_repo" => self.toasts.push(ToastSpec::info("Open repo (Phase 5)")),
            "close_tab" => self.close_tab(self.active_tab),
            "fetch" => self.toasts.push(ToastSpec::info("Fetch (Phase 4c)")),
            "pull" => self.toasts.push(ToastSpec::info("Pull (Phase 4c)")),
            "push" => self.toasts.push(ToastSpec::info("Push (Phase 4c)")),
            "commit" => self.commit(),
            "stage_all" => self.stage_all(),
            "unstage_all" => self.unstage_all(),
            "settings" => self.active_modal = Some(ActiveModal::Settings),
            "toggle_shortcut_bar" => {
                self.shortcut_bar_visible = !self.shortcut_bar_visible;
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

    /// Handle modal-only routes. Returns true if the key was a modal
    /// route (settings:* / modal:* / scrim dismiss) so the caller can
    /// short-circuit.
    fn handle_modal_route(&mut self, key: &str) -> bool {
        // Scrim outside-click dismiss for any modal.
        if key.ends_with(":dismiss") {
            self.active_modal = None;
            return true;
        }

        if let Some(rest) = key.strip_prefix("settings:") {
            self.handle_settings_route(rest);
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
            _ => false,
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
        }
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
            // calls.
            let paths: Vec<String> = t
                .status
                .unstaged
                .iter()
                .chain(t.status.untracked.iter())
                .map(|f| f.path.clone())
                .collect();
            for p in paths {
                t.repo.stage_file(&p)?;
            }
            Ok(())
        });
    }

    fn unstage_all(&mut self) {
        self.run_op("Unstage all", |t| {
            let paths: Vec<String> = t.status.staged.iter().map(|f| f.path.clone()).collect();
            for p in paths {
                t.repo.unstage_file(&p)?;
            }
            Ok(())
        });
    }

    fn commit(&mut self) {
        let active_idx = self.active_tab;
        let Some(tab) = self.tabs.get_mut(active_idx) else {
            return;
        };
        if tab.commit_subject.trim().is_empty() {
            self.toasts
                .push(ToastSpec::warning("Commit subject is empty"));
            return;
        }
        if tab.status.staged.is_empty() {
            self.toasts.push(ToastSpec::warning("No staged changes"));
            return;
        }
        let message = if tab.commit_body.trim().is_empty() {
            tab.commit_subject.clone()
        } else {
            format!("{}\n\n{}", tab.commit_subject.trim(), tab.commit_body.trim())
        };
        match tab.repo.commit(&message) {
            Ok(oid) => {
                tab.commit_subject.clear();
                tab.commit_body.clear();
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
    SidebarSection::ALL
        .iter()
        .copied()
        .find(|s| s.key() == key)
}

// ---------------------------------------------------------------------------
// Chrome composition
// ---------------------------------------------------------------------------

fn tab_bar(app: &WhisperApp) -> El {
    let mut tabs: Vec<El> = app
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| tab_chip(t.repo_name.clone(), i, i == app.active_tab))
        .collect();
    tabs.push(
        icon_button(IconName::Plus)
            .key("open_repo")
            .tooltip("Open repository (Ctrl+O)"),
    );
    row(tabs)
        .surface_role(SurfaceRole::Panel)
        .padding(Sides::xy(tokens::SPACE_SM, tokens::SPACE_XS))
        .gap(tokens::SPACE_XS)
        .align(Align::Center)
}

fn tab_chip(label: String, idx: usize, active: bool) -> El {
    let inner = row([
        text(label).label(),
        icon_button(IconName::X)
            .key(format!("tab_close:{idx}"))
            .tooltip("Close tab"),
    ])
    .gap(tokens::SPACE_XS)
    .align(Align::Center);

    let mut chip = inner
        .padding(Sides::xy(tokens::SPACE_SM, tokens::SPACE_XS))
        .key(format!("tab:{idx}"));
    if active {
        chip = chip.surface_role(SurfaceRole::Raised);
    }
    chip
}

fn header_bar(active: Option<&RepoTab>) -> El {
    let branch = match active {
        Some(t) => {
            let label = if t.current_branch.is_empty() {
                "(detached)".to_string()
            } else {
                t.current_branch.clone()
            };
            row([icon(IconName::GitBranch), text(label).label()])
                .gap(tokens::SPACE_XS)
                .align(Align::Center)
        }
        None => row([icon(IconName::GitBranch), text("(no repo)").muted()])
            .gap(tokens::SPACE_XS)
            .align(Align::Center),
    };

    let actions_enabled = active.is_some();

    row([
        branch,
        spacer(),
        button_with_icon(IconName::Download, "Fetch")
            .key("fetch")
            .tooltip("git fetch"),
        button_with_icon(IconName::Download, "Pull")
            .key("pull")
            .tooltip("git pull"),
        button_with_icon(IconName::Upload, "Push")
            .key("push")
            .tooltip("git push"),
        button_with_icon(IconName::GitCommit, "Commit")
            .key("commit")
            .primary()
            .tooltip("Stage and commit (Ctrl+Enter)"),
        icon_button(IconName::Settings)
            .key("settings")
            .tooltip("Settings"),
    ])
    .surface_role(SurfaceRole::Panel)
    .padding(Sides::xy(tokens::SPACE_LG, tokens::SPACE_SM))
    .gap(tokens::SPACE_SM)
    .align(Align::Center)
    .opacity(if actions_enabled { 1.0 } else { 0.85 })
}

fn shortcut_bar() -> El {
    row([
        kbd("Ctrl+O", "Open"),
        kbd("Ctrl+W", "Close tab"),
        kbd("Ctrl+/", "Toggle shortcuts"),
    ])
    .padding(Sides::xy(tokens::SPACE_LG, tokens::SPACE_XS))
    .gap(tokens::SPACE_LG)
    .align(Align::Center)
}

fn kbd(chord: &str, label: &str) -> El {
    row([badge(chord).muted(), text(label).caption()])
        .gap(tokens::SPACE_XS)
        .align(Align::Center)
}

fn main_placeholder(active: Option<&RepoTab>) -> El {
    let body = match active {
        Some(t) => column([
            h2(format!("{}", t.repo_name)),
            text(format!(
                "Branch: {} · {} local · {} remote · {} tags · {} stashes · {} worktrees · {} submodules",
                if t.current_branch.is_empty() {
                    "(detached)"
                } else {
                    &t.current_branch
                },
                t.local_branches().len(),
                t.remote_branches().iter().map(|(_, b)| b.len()).sum::<usize>(),
                t.tags.len(),
                t.stashes.len(),
                t.worktrees.len(),
                t.submodules.len(),
            ))
            .muted(),
            paragraph(
                "Phase 3 wires the branch sidebar from real git data. \
                 Staging / diff / graph still placeholders.",
            ),
        ]),
        None => column([
            h2("No repository open"),
            paragraph("Press Ctrl+O or click + in the tab bar to open a repository."),
        ]),
    };
    body.padding(tokens::SPACE_LG)
        .gap(tokens::SPACE_MD)
        .height(Size::Fill(1.0))
        .width(Size::Fill(1.0))
}
