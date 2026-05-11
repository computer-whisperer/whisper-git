//! MSDF seam smoke test — proves whether per-row `vector()` assets
//! tile cleanly at row boundaries before we rebuild the commit graph
//! around the new vector primitive.
//!
//! Renders the same lane geometry three ways, side by side, against a
//! light background so AA dropoff would show up as a visible band:
//!
//! 1. **Per-row assets, integer pixel-aligned heights** — what the
//!    real commit graph would produce. Each row owns its own
//!    `VectorAsset`; the verticals stop at the asset bbox edges with
//!    `LineCap::Butt`. If MSDF AA at the asset boundary dropped
//!    below full coverage we'd see a horizontal band every ROW_H
//!    pixels.
//!
//! 2. **Per-row assets, with bbox extended 1px past the row spacing**
//!    — fakes overlap by making each asset slightly taller than the
//!    advance. The geometry ends 1px inside the bbox bottom rather
//!    than at the edge, so AA falls off inside the row instead of
//!    at the seam.
//!
//! 3. **One big asset** — single `vector()` covering the same total
//!    height with the same path data. Baseline: no seams possible
//!    here, so it's the visual ground truth.
//!
//! Run: `cargo run --bin vector_smoke`. Writes
//! `out/vector_smoke.png` for inspection.

use std::path::PathBuf;

use aetna_core::tree::Color;
use aetna_core::vector::{PathBuilder, VectorAsset, VectorLineCap};
use aetna_core::{App, BuildCx, El, prelude::*};
use anyhow::Result;
use whisper_git::screenshot_mode;

const ROW_H: f32 = 28.0;
const COL_W: f32 = 72.0;
const NUM_ROWS: usize = 14;
const LINE_W: f32 = 2.0;
const LINE_COLOR: Color = Color::rgb(120, 200, 240);
const NODE_COLOR: Color = Color::rgb(244, 114, 182);

/// Where on this row, in {none, top, bottom, both, node}, the curve
/// from lane 0 to lane 1 reaches into. Drives the geometry the row
/// builds.
#[derive(Clone, Copy)]
enum RowKind {
    /// Pure lane vertical at lane 0.
    Lane0,
    /// Lane 0 vertical + a node circle on lane 0.
    Lane0Node,
    /// Lane 0 outgoing curve to lane 1 (top half of the S).
    Lane0OutgoingToLane1,
    /// Lane 1 incoming curve from lane 0 (bottom half) + node on lane 1.
    Lane1IncomingNode,
    /// Lane 1 vertical only.
    Lane1,
    /// Lane 0 + Lane 1 verticals (both lanes active through this row).
    BothLanes,
}

const ROW_KINDS: [RowKind; NUM_ROWS] = [
    RowKind::Lane0,
    RowKind::Lane0,
    RowKind::Lane0Node,
    RowKind::Lane0OutgoingToLane1,
    RowKind::Lane1IncomingNode,
    RowKind::BothLanes,
    RowKind::BothLanes,
    RowKind::Lane1,
    RowKind::Lane1,
    RowKind::Lane1,
    RowKind::BothLanes,
    RowKind::Lane0,
    RowKind::Lane0,
    RowKind::Lane0Node,
];

fn lane_x(lane: usize) -> f32 {
    let lane_w = COL_W * 0.5;
    lane as f32 * lane_w + lane_w * 0.5
}

/// Build the geometry for one row at the given vertical extent. `y0`
/// and `y1` are the top and bottom of the row inside whatever
/// coordinate system the caller is composing in.
fn build_row_paths(kind: RowKind, y0: f32, y1: f32) -> Vec<aetna_core::vector::VectorPath> {
    let mut paths = Vec::new();
    let mid_y = (y0 + y1) * 0.5;
    let h = y1 - y0;

    let vert = |lane: usize| {
        PathBuilder::new()
            .move_to(lane_x(lane), y0)
            .line_to(lane_x(lane), y1)
            .stroke_solid(LINE_COLOR, LINE_W)
            .stroke_line_cap(VectorLineCap::Butt)
            .build()
    };
    let node = |lane: usize| {
        let r = 5.0;
        PathBuilder::new()
            .move_to(lane_x(lane) + r, mid_y)
            .cubic_to(
                lane_x(lane) + r,
                mid_y - r * 0.55,
                lane_x(lane) + r * 0.55,
                mid_y - r,
                lane_x(lane),
                mid_y - r,
            )
            .cubic_to(
                lane_x(lane) - r * 0.55,
                mid_y - r,
                lane_x(lane) - r,
                mid_y - r * 0.55,
                lane_x(lane) - r,
                mid_y,
            )
            .cubic_to(
                lane_x(lane) - r,
                mid_y + r * 0.55,
                lane_x(lane) - r * 0.55,
                mid_y + r,
                lane_x(lane),
                mid_y + r,
            )
            .cubic_to(
                lane_x(lane) + r * 0.55,
                mid_y + r,
                lane_x(lane) + r,
                mid_y + r * 0.55,
                lane_x(lane) + r,
                mid_y,
            )
            .fill_solid(NODE_COLOR)
            .build()
    };
    // Outgoing curve from lane `a` to lane `b` over the row range
    // (vertical tangent at the top, curving toward the destination
    // lane near the bottom). Mirrors the pre-port S-shape.
    let outgoing = |a: usize, b: usize| {
        PathBuilder::new()
            .move_to(lane_x(a), y0)
            .cubic_to(
                lane_x(a),
                y0 + h * 0.4,
                lane_x(b),
                y0 + h * 0.6,
                lane_x(b),
                y1,
            )
            .stroke_solid(LINE_COLOR, LINE_W)
            .stroke_line_cap(VectorLineCap::Butt)
            .build()
    };

    match kind {
        RowKind::Lane0 => paths.push(vert(0)),
        RowKind::Lane0Node => {
            paths.push(vert(0));
            paths.push(node(0));
        }
        RowKind::Lane0OutgoingToLane1 => {
            // Lane 0 stays vertical (parent continues straight down).
            paths.push(vert(0));
            // Outgoing branch from lane 0 to lane 1 (this row is
            // the top half of the S-curve).
            paths.push(outgoing(0, 1));
        }
        RowKind::Lane1IncomingNode => {
            // Incoming from lane 0 (bottom half of the S — curve from
            // the upstream row meeting this row's top with vertical
            // tangent, settling at lane 1 by mid_y where the node
            // sits).
            paths.push(
                PathBuilder::new()
                    .move_to(lane_x(0), y0)
                    .cubic_to(
                        lane_x(0),
                        y0 + h * 0.4,
                        lane_x(1),
                        y0 + h * 0.6,
                        lane_x(1),
                        mid_y,
                    )
                    .stroke_solid(LINE_COLOR, LINE_W)
                    .stroke_line_cap(VectorLineCap::Butt)
                    .build(),
            );
            // Lane 1 continues from the node down to row bottom.
            paths.push(
                PathBuilder::new()
                    .move_to(lane_x(1), mid_y)
                    .line_to(lane_x(1), y1)
                    .stroke_solid(LINE_COLOR, LINE_W)
                    .stroke_line_cap(VectorLineCap::Butt)
                    .build(),
            );
            paths.push(node(1));
        }
        RowKind::Lane1 => paths.push(vert(1)),
        RowKind::BothLanes => {
            paths.push(vert(0));
            paths.push(vert(1));
        }
    }
    paths
}

/// Per-row asset whose view_box matches its row exactly — strict
/// abut, no overdraw at the seam.
fn per_row_asset(kind: RowKind) -> VectorAsset {
    let paths = build_row_paths(kind, 0.0, ROW_H);
    VectorAsset::from_paths([0.0, 0.0, COL_W, ROW_H], paths)
}

/// Per-row asset whose view_box extends 1px past the row advance, so
/// the asset's bottom edge sits inside what would be the next row's
/// top pixel. Combined with `paint_overflow`, the rasterised content
/// overlaps the next row's top pixel by 1px.
fn per_row_asset_overdrawn(kind: RowKind) -> VectorAsset {
    let extra = 1.0;
    let paths = build_row_paths(kind, 0.0, ROW_H + extra);
    VectorAsset::from_paths([0.0, 0.0, COL_W, ROW_H + extra], paths)
}

/// Single asset spanning the full column — the baseline. No seams
/// possible.
fn full_column_asset() -> VectorAsset {
    let mut all = Vec::new();
    for (i, kind) in ROW_KINDS.iter().enumerate() {
        let y0 = i as f32 * ROW_H;
        let y1 = y0 + ROW_H;
        all.extend(build_row_paths(*kind, y0, y1));
    }
    VectorAsset::from_paths([0.0, 0.0, COL_W, NUM_ROWS as f32 * ROW_H], all)
}

fn per_row_column(overdrawn: bool) -> El {
    let rows: Vec<El> = ROW_KINDS
        .iter()
        .map(|kind| {
            let asset = if overdrawn {
                per_row_asset_overdrawn(*kind)
            } else {
                per_row_asset(*kind)
            };
            // For the overdrawn variant the asset is 1px taller than
            // the row advance — paint_overflow keeps the visual without
            // affecting layout, so the rasterised content overlaps the
            // next row's top pixel.
            let mut el = vector(asset)
                .width(Size::Fixed(COL_W))
                .height(Size::Fixed(ROW_H));
            if overdrawn {
                el = el.paint_overflow(Sides {
                    left: 0.0,
                    right: 0.0,
                    top: 0.0,
                    bottom: 1.0,
                });
            }
            el
        })
        .collect();
    column(rows).gap(0.0).align(Align::Stretch)
}

fn full_column() -> El {
    let asset = full_column_asset();
    vector(asset)
        .width(Size::Fixed(COL_W))
        .height(Size::Fixed(NUM_ROWS as f32 * ROW_H))
}

fn labelled(label: &str, body: El) -> El {
    column([text(label.to_string()).caption().muted(), body])
        .gap(tokens::SPACE_2)
        .align(Align::Center)
}

struct SmokeApp;

impl App for SmokeApp {
    fn build(&self, _cx: &BuildCx) -> El {
        column([
            text("MSDF per-row vs single-asset seam check").label(),
            text(
                "If row seams produce a visible coverage gap, the left column \
                 will show horizontal bands every ROW_H pixels and the rightmost \
                 column will look continuous.",
            )
            .caption()
            .muted()
            .text_wrap(TextWrap::Wrap),
            row([
                labelled("per-row, strict abut", per_row_column(false)),
                labelled("per-row, +1px overlap", per_row_column(true)),
                labelled("single asset (baseline)", full_column()),
            ])
            .gap(tokens::SPACE_6)
            .align(Align::Start),
        ])
        .gap(tokens::SPACE_4)
        .padding(tokens::SPACE_5)
        .width(Size::Fill(1.0))
        .height(Size::Fill(1.0))
        .fill(tokens::BACKGROUND)
    }
}

fn main() -> Result<()> {
    let out = PathBuf::from("out/vector_smoke.png");
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    screenshot_mode::run(&out, 2160, 1680, 3.0, SmokeApp)?;
    println!("wrote {}", out.display());
    Ok(())
}
