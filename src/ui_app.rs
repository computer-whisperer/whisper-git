//! Phase 2 placeholder App impl: tab bar + header + shortcut bar +
//! placeholder main area, wired into stock aetna widgets.
//!
//! Real per-repo state (commits, branches, staging, diff) lands in
//! later phases; here we just exercise event routing and the chrome
//! visual story.

use std::path::PathBuf;

use aetna_core::{
    App, AppShader, BuildCx, El, IconName, KeyChord, UiEvent, UiEventKind,
    prelude::*,
    toast::ToastSpec,
};

/// commit_node.wgsl — copied verbatim from the aetna `custom_paint`
/// example. Per-row commit-graph cell: vertical lane line + circle node.
/// Registered up front so Phase 6 doesn't need to retrofit shader
/// wiring into the host.
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
    pub repos: Vec<PathBuf>,
    pub active_tab: usize,
    pub shortcut_bar_visible: bool,
    pub toasts: Vec<ToastSpec>,
}

impl WhisperApp {
    pub fn new(repos: Vec<PathBuf>) -> Self {
        Self {
            repos,
            active_tab: 0,
            shortcut_bar_visible: true,
            toasts: Vec::new(),
        }
    }

    fn active_repo(&self) -> Option<&PathBuf> {
        self.repos.get(self.active_tab)
    }

    fn repo_label(path: &PathBuf) -> String {
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    }
}

impl App for WhisperApp {
    fn build(&self, _cx: &BuildCx) -> El {
        let mut chrome: Vec<El> = Vec::with_capacity(4);
        if self.repos.len() >= 1 {
            chrome.push(tab_bar(&self.repos, self.active_tab));
        }
        chrome.push(header_bar(self.active_repo()));
        if self.shortcut_bar_visible {
            chrome.push(shortcut_bar());
        }
        chrome.push(main_placeholder(self.active_repo()));

        let main = column(chrome).gap(0.0);
        // overlays(...) wrapper required so the runtime has somewhere
        // to append toast / tooltip layers.
        overlays(main, [])
    }

    fn on_event(&mut self, event: UiEvent) {
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

    fn hotkeys(&self) -> Vec<(KeyChord, String)> {
        vec![
            (KeyChord::ctrl('o'), "open_repo".to_string()),
            (KeyChord::ctrl('w'), "close_tab".to_string()),
            // F1 / question-mark would be conventional, but Ctrl+/
            // composes from one keychord (no Shift coupling) and
            // doesn't collide with text-input focus.
            (KeyChord::ctrl('/'), "toggle_shortcut_bar".to_string()),
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
        // Tab switching: keys are formatted "tab:{idx}" / "tab_close:{idx}".
        if let Some(idx_str) = key.strip_prefix("tab_close:") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                self.close_tab(idx);
            }
            return;
        }
        if let Some(idx_str) = key.strip_prefix("tab:") {
            if let Ok(idx) = idx_str.parse::<usize>()
                && idx < self.repos.len()
            {
                self.active_tab = idx;
            }
            return;
        }

        match key {
            "open_repo" => self
                .toasts
                .push(ToastSpec::info("Open repo (Phase 5)")),
            "close_tab" => self.close_tab(self.active_tab),
            "fetch" => self.toasts.push(ToastSpec::info("Fetch (Phase 4)")),
            "pull" => self.toasts.push(ToastSpec::info("Pull (Phase 4)")),
            "push" => self.toasts.push(ToastSpec::info("Push (Phase 4)")),
            "commit" => self
                .toasts
                .push(ToastSpec::info("Commit (Phase 4)")),
            "settings" => self
                .toasts
                .push(ToastSpec::info("Settings (Phase 5)")),
            "toggle_shortcut_bar" => {
                self.shortcut_bar_visible = !self.shortcut_bar_visible;
            }
            _ => {}
        }
    }

    fn close_tab(&mut self, idx: usize) {
        if idx >= self.repos.len() {
            return;
        }
        self.repos.remove(idx);
        if self.active_tab >= self.repos.len() && !self.repos.is_empty() {
            self.active_tab = self.repos.len() - 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Chrome composition
// ---------------------------------------------------------------------------

fn tab_bar(repos: &[PathBuf], active: usize) -> El {
    let mut tabs: Vec<El> = repos
        .iter()
        .enumerate()
        .map(|(i, p)| tab_chip(WhisperApp::repo_label(p), i, i == active))
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

fn header_bar(active_repo: Option<&PathBuf>) -> El {
    let branch = match active_repo {
        Some(p) => row([
            icon(IconName::GitBranch),
            text(WhisperApp::repo_label(p)).label(),
        ])
        .gap(tokens::SPACE_XS)
        .align(Align::Center),
        None => row([icon(IconName::GitBranch), text("(no repo)").muted()])
            .gap(tokens::SPACE_XS)
            .align(Align::Center),
    };

    let actions_enabled = active_repo.is_some();

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

fn main_placeholder(active_repo: Option<&PathBuf>) -> El {
    let body = match active_repo {
        Some(p) => column([
            h2(format!("Active repo: {}", WhisperApp::repo_label(p))),
            text(p.display().to_string()).mono().muted(),
            paragraph(
                "Phase 2 only renders chrome. Branch sidebar (Phase 3), staging \
                 well + diff (Phase 4), commit graph (Phase 6), and dialogs \
                 (Phase 5) are still placeholders.",
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
}
