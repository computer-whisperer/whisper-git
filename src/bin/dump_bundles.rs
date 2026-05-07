//! Dump aetna bundle artifacts (svg + tree + draw_ops + lint +
//! shader_manifest) for whisper-git's scenes. CPU-only: no GPU, no
//! window. Faster than `--screenshot` and catches layout regressions.
//!
//! Phase 0: one scene (the placeholder). New scenes get added as views
//! land in later phases. The vulkano `--screenshot` path remains the
//! authority for shader output; this path is the layout net.

use std::path::PathBuf;

use aetna_core::{App, BuildCx, Rect, render_bundle, write_bundle};
use anyhow::{Context, Result};

use whisper_git::WhisperApp;

fn main() -> Result<()> {
    let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("out");
    let viewport = Rect::new(0.0, 0.0, 1600.0, 900.0);

    let scenes: &[(&str, fn() -> WhisperApp)] = &[
        ("placeholder", || WhisperApp::new(Vec::new())),
        ("placeholder_with_repo", || {
            WhisperApp::new(vec![PathBuf::from("/example/repo")])
        }),
    ];

    let mut total_findings = 0;
    for (name, build) in scenes {
        let app = build();
        let theme = app.theme();
        let cx = BuildCx::new(&theme);
        let mut tree = app.build(&cx);
        let bundle = render_bundle(&mut tree, viewport, Some(env!("CARGO_PKG_NAME")));
        let written = write_bundle(&bundle, &out_dir, name).context("write_bundle")?;
        for p in &written {
            println!("wrote {}", p.display());
        }
        if !bundle.lint.findings.is_empty() {
            eprintln!("\nlint findings ({} in {name}):", bundle.lint.findings.len());
            eprint!("{}", bundle.lint.text());
            total_findings += bundle.lint.findings.len();
        }
    }

    if total_findings > 0 {
        eprintln!("\n{total_findings} total lint findings");
    }
    Ok(())
}
