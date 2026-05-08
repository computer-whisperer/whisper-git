# whisper-git → aetna UI port

## What this is

A long-running rewrite of whisper-git's UI off its bespoke vulkano widget
stack onto the [aetna](https://github.com/aetna-ui/aetna) UI toolkit
(also authored by the project maintainer). The port lives on the
`aetna-ui` branch; `main` is the working pre-port app.

**Backend is preserved verbatim.** `src/git/` (libgit2 wrapper, refs,
status, hunk, diff, async ops) is byte-identical to the pre-port version
modulo the small re-exports needed by the new code path. The port is
purely a UI rewrite — never let it slip into a backend rebuild.

The renderer also stays vulkano: aetna ships `aetna-vulkano::Runner`,
which paints aetna's tree on top of our existing Vulkan device.

## Why

Aetna reached the maturity to host a real app, and whisper-git was always
intended to be the canonical native port. Doing the port has been a
forcing function for aetna polish: a number of breaking changes upstream
(token rename to numeric scale, `Side` parameter on `apply_event_fixed`,
`AppShader::samples_time`) shipped because something in this repo asked
for them.

## Where things live

```
aetna-ui/
├── Cargo.toml                  # aetna path-dep at ~/workspace/aetna/aetna.main
├── src/
│   ├── lib.rs                  # the active module set (everything below `pub mod`)
│   ├── main.rs                 # CLI entry + screenshot-state injection
│   ├── git/                    # PRESERVED — libgit2 wrapper + async git CLI ops
│   │   ├── mod.rs              # GitRepo, CommitInfo, BranchTip, TagInfo, …
│   │   ├── async_ops.rs        # fetch/push/pull/clone/cherry-pick spawning
│   │   ├── status.rs           # WorkingDirStatus + stage/unstage
│   │   ├── refs.rs             # branch/tag/worktree enumeration
│   │   ├── diff.rs             # DiffFile, DiffHunk, DiffLine
│   │   └── hunk.rs             # selective hunk staging
│   ├── ui_app.rs               # WhisperApp impl App; modal/route dispatch
│   ├── repo_tab.rs             # RepoTab + WorktreeView per-tab data
│   ├── host.rs                 # winit host + cursor wiring
│   ├── screenshot_mode.rs      # offscreen render path for headless PNG
│   ├── sidebar.rs              # branch sidebar composition
│   ├── staging.rs              # staging well + worktree pill bar
│   ├── diff_view.rs            # working-dir diff viewer
│   ├── commit_graph.rs         # history view + lane layout + pill set
│   ├── commit_details.rs       # commit detail right pane
│   ├── dialogs.rs              # modal builders (Settings / Confirm / Error / Clone / Token)
│   ├── config.rs               # settings.json persistence
│   ├── token_store.rs          # PRESERVED — system keychain integration
│   ├── crash_log.rs            # PRESERVED
└── docs/aetna-port.md          # this file
```

`lib.rs` lists the entire compiled surface. The pre-port `app_*.rs`,
`async_polling.rs`, `views/`, `ui/`, `messages/`, `rendering.rs`,
`submodule_nav.rs`, `watcher.rs`, `ai.rs`, `ci.rs`, `github.rs`, and
`gitlab.rs` files were deleted in Phase 7a; if you need a prior
implementation as reference, retrieve it from git history
(`git show HEAD~N:src/views/welcome.rs`).

## Phase status

The phase numbering is mostly historical at this point. Done:

- **Phase 0** — placeholder app, host shell, screenshot mode
- **Phase 2–3** — `RepoTab` opens, branch sidebar wired to real git data
- **Phase 4a** — staging well UI + controlled commit-message editor
- **Phase 4b** — working-dir diff viewer with hunk Stage/Unstage
- **Phase 4c** — wire stage/unstage/commit to real git ops
- **Phase 5a** — modal infrastructure (Settings / Confirm / Error)
- **Phase 5b** — sidebar context menus + branch/tag/stash delete confirms
- **Phase 5c** — Open / Clone / Manage-Tokens dialogs
- **Phase 6** — commit graph history view + commit details pane
- **Phase 6 follow-up** — commit-row context menu
- **Aetna 0.3.0 catch-up** — token rename, shader API
- **Polish pass** — semantic surfaces (per `AETNA_CORRECTION.md` then-deleted)
- **Async ops slice 1** — Fetch / Push / Cherry-pick / Revert
- **Per-worktree state model** — `WorktreeView` + worktree selector pill bar
- **Synthetic + orphan commit injection** — restored from old whisper-git
- **Commit-row pills** — branch / tag / HEAD / clean-worktree (WT:) / ORPHAN
- **Resizable sidebars** — left/right with `resize_handle` + persisted widths
- **OS cursor wiring** — propagate aetna's resolved cursor to winit
- **Aetna upstream catch-up** — numeric `SPACE_*`, `Side::Start/End` for resize
- **Phase 7a** — delete legacy `ui/`, `views/`, `app_*.rs`, `messages/`,
  `renderer/`, `watcher.rs`, etc.; prune unused Cargo deps

Deferred / pending (no fixed order):

- **Phase 7b** — Welcome view (port from `views/welcome.rs` in git
  history; wire new logo from `assets/`)
- **Async slice 2** — Pull / Merge / Rebase / Force-push (need branch pickers)
- **Variable-height virtual list** — needs aetna-core changes; deferred upstream
- **Token dialog GitLab multi-host** — re-enable when `gitlab.rs` is ported
- **Watcher + filesystem refresh** — currently synchronous on user action only
- **Header progress affordance** — disabled-while-running, spinner, 60s warning
- **Submodule drill-down navigation** — old `submodule_nav.rs` is uncompiled

## Architectural decisions worth knowing

### Per-worktree state, not per-tab

A long-standing pre-port distinction that briefly got lost in the early
port: anything that's logically a property of a *specific working tree*
— `status`, `current_branch`, `head_oid`, the commit-message draft, the
file under preview — lives on `WorktreeView`, not on `RepoTab`. Each
tab carries `worktree_views: HashMap<PathBuf, WorktreeView>` plus an
`active_worktree: Option<PathBuf>` that the staging well + diff pane
key off.

Switching worktrees is a real first-class operation: `RepoTab::select_worktree(path)`
opens the worktree's own `GitRepo` handle (cached), refreshes its
status, and re-rewrites `branch_tips[*].is_head` against the new
worktree's HEAD. Stage / unstage / commit / hunk all run against
`tab.active_repo()`, not `tab.repo`, so they apply to the right tree.

The pill bar above the staging well drives this from the UI side; the
sidebar Worktrees section also routes through the same handler. Naming
is consistent across both (directory basename), so the same path always
shows the same label.

### Catalog widgets first; manual surfaces are a smell

When the aetna team flagged our first polish pass as "routing around the
issue" (the `AETNA_CORRECTION.md` note), the lesson was: reach for
`card()` / `card_content()` / `widgets::sidebar::sidebar()` /
`toolbar()` / `tabs_list()` / `tab_trigger()` *first*, and only fall
back to manual `.fill(CARD).stroke(BORDER)` composition when the catalog
shape genuinely doesn't fit (e.g. tab chips that need a sibling Close
button alongside a `tab_trigger`).

The lint pipeline is the verification gate. After any polish, run:

```sh
cargo run --release --bin dump_bundles
grep -c MissingSurfaceFill out/*.lint.txt
```

Zero findings is the bar. The lint catches "decorative" surface roles
(`Panel`, `Raised`, `Popover`, `Danger`) used without an explicit fill,
which used to leak through as widgets-on-black.

See `feedback_aetna_surface_painting.md` in memory for the full
decision tree of which catalog widget to reach for in which situation.

### Synthetic + orphan commits

The History view's commit list isn't just `repo.commit_graph(N)`. Each
refresh:

1. Calls `commit_graph_with_orphans` (falls back to `commit_graph` on
   reflog read errors) so unreachable work — finished rebases, dropped
   branches — appears as `orphan` rows rather than disappearing.
2. Walks `worktree_views` and emits a synthetic "uncommitted changes"
   row per dirty worktree (sentinel oid, amber lane node, WT: pill).
   Slotted in by mtime via `insert_synthetics_sorted`.
3. Rebuilds the lane layout over the merged list so synthetics inherit
   their parent's lane.

The new `RepoTab::build_synthetic_entries` walks views directly rather
than the old `git::create_synthetic_entries(&worktrees)` path; the old
helper silently skipped the main worktree in multi-worktree setups.

### Async ops via mpsc + EventLoopProxy

Every async git op (fetch/push/pull/clone/cherry-pick/revert/etc.)
follows the same shape:

1. Spawn a thread that runs the git CLI with `GIT_TERMINAL_PROMPT=0`,
   captures stdout/stderr, sends a result over an mpsc channel.
2. After sending, call `proxy.send_event(())` to wake winit.
3. Park the receiver on a per-tab slot: `fetch_op` / `pull_op` /
   `push_op` / `mutation_op` (cherry-pick + revert share, since they
   conflict). Clone is app-scoped (`WhisperApp::clone_op`) since the
   new repo doesn't have a tab yet.
4. `App::before_build` calls `poll_async_ops`, which `try_recv`s every
   slot, refreshes the tab on completion, and emits a success toast or
   pops an Error modal with the captured stderr (run through
   `classify_git_error` for user-friendly wording).

The proxy is injected via a setup closure on `host::run`:

```rust
host::run("Whisper Git", viewport, app, |a, p| { a.proxy = Some(p); })?;
```

`None` for headless use (`with_tabs` / dump_bundles); attempting to start
an op without a proxy emits an error toast.

### Modals with inline form state

For dialogs that own input state (Clone, Token), the form lives inline
on the `ActiveModal` variant:

```rust
pub enum ActiveModal {
    Settings,
    Confirm { title, body, ok_label, destructive, action },
    Error { title, body },
    Clone(CloneForm),
    Token(TokenForm),
}
```

`on_event` routes `text_input::apply_event` into the form's fields by
key (`clone:url`, `clone:dest`, `token:github`). This keeps modal state
self-contained and makes "esc-clear" dead simple — drop the variant.

### Resizable sidebars

`resize_handle::resize_handle(Axis::Row).key("sidebar:resize")` drops
between siblings inside the body row. `apply_event_fixed(value, drag,
event, key, axis, side, min, max)` folds the drag back into the width.
`Side::Start` for the left sidebar, `Side::End` for the right pane (so
drag-left grows the right pane). Widths persist via `Config` and write
on PointerUp, not per-tick.

### OS cursor

Aetna resolves the pointer cursor through the layout tree
(`UiState::cursor(&root)` walks from hovered/pressed up to the nearest
declaration). `host.rs` queries this each frame after `runner.prepare`
and feeds the result through `winit_cursor` (a 1:1 enum map) into
`window.set_cursor(...)`. winit dedupes internally, so the unconditional
per-frame call is cheap.

We inline the mapper rather than depending on `aetna-winit-wgpu` (which
brings wgpu as a transitive dep just for that one function).

## Verification surface

Three cross-checks that catch regressions before they ship:

### `dump_bundles` — golden tree dumps

```sh
cargo run --release --bin dump_bundles
```

Writes `*.svg` (visual), `*.tree.txt` (laid-out tree with rects), and
`*.lint.txt` (Aetna's `lint` checker output) per scene under `out/`.
Scenes cover sidebar, working diff, history, modals, context menus,
multi-tab. Adding a new modal/scene means adding a `WhisperApp::with_tabs(...)`
fixture in `bin/dump_bundles.rs`.

### `MissingSurfaceFill` lint

```sh
grep -c MissingSurfaceFill out/*.lint.txt | grep -v ':0$'
```

Empty result = clean. Any finding means a decorative `surface_role`
(Panel / Raised / Popover / Danger) is being used without a paired
`.fill(...)` — a "widgets on black" bug waiting to happen.

### Screenshot mode

```sh
cargo run --release --bin whisper-git -- \
    --screenshot /tmp/foo.png --size 1600x900 --repo PATH \
    [--screenshot-state STATE]
```

States: `history`, `diff`, `settings`, `confirm`, `error`, `clone`,
`token`, `token-edit`, `commit-menu`, `context-menu`. Add new ones in
`main.rs::apply_screenshot_state` for new modal/scene checks.

The vulkano offscreen path coexists with bundle dumps — they verify
different things (shader-correct rendering vs tree shape + lint).

## Tracking aetna upstream

Aetna is at `~/workspace/aetna/aetna.main` as a path dep. Versions move
quickly. When the build breaks after a `git pull` over there, common
fixes:

- **Token renames**: aetna periodically reshapes `tokens::*`. The recent
  one was `SPACE_XS/SM/MD/LG/XL → SPACE_1..SPACE_12`. `cargo build`'s
  E0425 errors point at the call sites; `sed` across `src/*.rs`.
- **Widget signature changes**: e.g. `apply_event_fixed` gained a `Side`
  parameter, `register_shader_with` gained `samples_time`, `AppShader`
  gained `samples_time` field. Mechanical fixes; the new arg is usually
  the obvious default for our case.
- **Catalog reshuffles**: e.g. `tabs_list` doesn't accept per-option
  children — when the catalog widget can't express what we need, fall
  back to manual composition over `tab_trigger` + sibling row.

Calibrated reference patterns to consult when something visual feels
off: `~/workspace/aetna/aetna.main/crates/aetna-core/examples/dashboard_01_calibration.rs`,
`polish_calibration.rs`, `density_calibration.rs`.

## Memory pointers

The auto-memory under
`~/.claude/projects/-home-christian-workspace-whisper-git-whisper-git-git/memory/`
carries the cross-session decisions that don't belong in code comments:

- `project_aetna_port.md` — high-level project framing
- `feedback_aetna_upstream.md` — when something's broken, fix aetna,
  don't hack around it here
- `feedback_aetna_surface_painting.md` — catalog-first widget choices,
  surface-role taxonomy, lint verification gate
- `feedback_theme_default.md` — drop the old palette, use aetna's stock
  dark theme (currently Radix slate-blue dark)
- `project_screenshot_pipeline.md` — keep vulkano PNG path alongside
  bundle dumps for shader verification
