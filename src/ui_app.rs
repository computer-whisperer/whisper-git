//! Phase 0 placeholder App impl. Real views land in later phases.
//!
//! Registers the `commit_node` shader up front so the commit graph
//! (Phase 6) doesn't need to retrofit shader registration into the host
//! later. The shader text is the prototype pulled from
//! `aetna/examples/src/bin/custom_paint.rs`; whisper-git will iterate on
//! it when the real graph lands.

use std::path::PathBuf;

use aetna_core::{App, AppShader, BuildCx, El, prelude::*};

/// commit_node.wgsl — copied verbatim from the aetna `custom_paint`
/// example. Per-row commit-graph cell: vertical lane line + circle node.
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
}

impl WhisperApp {
    pub fn new(repos: Vec<PathBuf>) -> Self {
        Self { repos }
    }
}

impl App for WhisperApp {
    fn build(&self, _cx: &BuildCx) -> El {
        let repo_line = if self.repos.is_empty() {
            text("(no repos passed on the command line)").muted()
        } else {
            text(
                self.repos
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
            .mono()
        };

        column([
            h1("Whisper Git"),
            paragraph("Aetna UI port — Phase 0 placeholder"),
            text("Repositories:").label(),
            repo_line,
        ])
        .padding(tokens::SPACE_LG)
        .gap(tokens::SPACE_MD)
    }

    fn shaders(&self) -> Vec<AppShader> {
        vec![AppShader {
            name: "commit_node",
            wgsl: COMMIT_NODE_WGSL,
            samples_backdrop: false,
        }]
    }
}
