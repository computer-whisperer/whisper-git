# Working on aetna-ui (whisper-git)

This UI is built on the [aetna](https://crates.io/crates/aetna-core) library.
A previous session got stuck on the "invisible panel" problem and the eventual
fix (commit 55cef9f) routed around the actual issue — manually composing
`column(...).fill(tokens::CARD).stroke(tokens::BORDER)` for every pane wrapper
instead of reaching for the catalog widgets that already do this.

This file explains what to do differently.

## Reach for catalog widgets first

Aetna ships a deliberate catalog mirroring shadcn / WAI-ARIA shapes. Before
composing primitives, check the **Reach for these first** table in the
[aetna-core docs](https://docs.rs/aetna-core) — every "panel-shape" pattern in
this repo currently bypasses an existing widget:

| Current (this repo) | Use instead |
|---|---|
| `src/sidebar.rs::sidebar` — `scroll([...]).fill(CARD).stroke(BORDER)` wrapper | `sidebar([sidebar_header(...), sidebar_group([...])])` from `aetna_core::prelude` |
| `src/ui_app.rs::tab_bar` + `tab_chip` (manual MUTED row + per-chip `.selected()`) | `tabs_list(key, &current, options)` + `tab_trigger` |
| `src/ui_app.rs::view_mode_segment` (`row([button, button]).fill(MUTED)`) | `tabs_list` — its docstring is literally "a segmented control row of tab triggers" |
| `src/ui_app.rs::header_bar` (manual `row(...).fill(CARD).stroke(BORDER)`) | `toolbar([toolbar_title(...), spacer(), toolbar_group([...])])` (wrap in `card()` if you want chrome) |
| `src/diff_view.rs::diff_view`, `src/diff_view.rs::hunk_block`, `src/staging.rs::staging_well`, `src/commit_details.rs::commit_details_pane`, `src/commit_graph.rs::history_view` — all manually compose `column(...).fill(CARD).stroke(BORDER)` for pane wrappers | `card([card_content([...])])` — bundles `surface_role(Panel) + fill(CARD) + stroke(BORDER) + radius(MD) + shadow(MD)`, the dropped radius/shadow being the visible regression in the current code |

**Wrap, don't replace.** When `sidebar_menu_button_with_icon` doesn't fit
your row anatomy (count badges on group headers, sub-grouped remotes,
HEAD-vs-selected mixed with custom leading icons — i.e., everything in
`src/sidebar.rs::section_block`), **keep the outer `sidebar([...])` for the
panel surface and compose the rows freely inside.** Same for `card_content`
— anything column-shaped goes there. The outer widget is what gives you the
canonical fill + stroke + radius + shadow recipe.

## What went wrong with `surface_role(Panel)`

`SurfaceRole::Panel` is a *decorative* role — it sets stroke + shadow only,
**not a fill**. So `column(...).surface_role(SurfaceRole::Panel)` paints a
thin border + soft shadow over `BACKGROUND` and reads as "invisible." The
prior fix discovered this and switched to manual `fill(CARD) + stroke(BORDER)`
on every wrapper, which works but loses the radius and shadow that `card()`
provides — and reinvents what the catalog already packaged.

The aetna lint pipeline now flags this directly as `MissingSurfaceFill` —
visible in the `*.lint.txt` output of any bundle dump. Run the screenshot /
artifact dump pipeline (`bin/dump_bundles`) and check the lint output:
findings categorized as `MissingSurfaceFill` mean a panel-shape node is
missing its fill, and the suggested fix is `card()` / `sidebar()` /
`dialog()`.

`SurfaceRole` variants are now documented per-variant in aetna-core: each
one is tagged *Decorative* (Panel / Raised / Popover / Danger — bring your
own fill) or *Fill-providing* (Sunken / Selected / Current / Input). For
selected / current state on rows, prefer the `.selected()` / `.current()`
chainables — they set fill + stroke + content color in one call, which is
what the current code (post 55cef9f) already does correctly.

## Concrete refactor

The smallest lossless step is to wrap each existing pane composition in the
right catalog widget without disturbing the inner content:

1. `src/sidebar.rs::sidebar` — replace the outer `scroll([...]).fill(CARD).stroke(BORDER)` with `sidebar([scroll([body])])` (or fold the scroll into a single `sidebar([...])` if the section list fits — `sidebar` already handles overflow). Keep the section composition inside as-is.
2. `src/ui_app.rs::tab_bar` — replace with `tabs_list("tabs", &active_tab_key, app.tabs.iter().enumerate().map(|(i, t)| (i.to_string(), t.repo_name.clone())))`. The "+" open-repo button stays as a sibling next to `tabs_list`, not inside it.
3. `src/ui_app.rs::view_mode_segment` — same swap to `tabs_list("view", &current.to_str(), [("working", "Working"), ("history", "History")])`.
4. `src/ui_app.rs::header_bar` — wrap the contents in `toolbar([...])` and put that inside `card([card_content([toolbar(...)])])` if you want the header chrome (CARD fill + border + shadow).
5. `src/diff_view.rs`, `src/staging.rs`, `src/commit_details.rs`, `src/commit_graph.rs` pane wrappers — replace `column([...]).fill(CARD).stroke(BORDER).width(...).height(...)` with `card([card_content([...])]).width(...).height(...)`.

Keep `.fill(MUTED)` on the inner band-style headers (file-section headers,
tab strip background, recessed segment track) — that's the right surface
there. The `.selected()` / `.current()` chainables on individual rows in
`commit_graph.rs` and `sidebar.rs` are also already correct.

After the refactor, run `cargo run -p whisper-git --bin dump_bundles` (or
whatever the local dump binary is) and confirm the `*.lint.txt` output
contains no `MissingSurfaceFill` findings.
