# Async Engine Redesign

**Status:** Shipped + verified (2026-05-10) — six commits across `653d270…d7bee1a`.
Headless smoke (screenshot pipeline + dump_bundles + tests) green;
live behavior on a giant submoduled repo confirmed by hand.
**Branch:** `aetna-ui`

## Background

In Phase 7a (commit `4fbfbc1`, May 8 2026) the pre-port async/watcher
modules were deleted as "fossil code on disk for reference." That was
wrong: the modules encoded a design — earned through real iteration
against real failure modes — that the new architecture still needs.
After the deletion, the `aetna-ui` branch had no filesystem watcher,
ran git queries synchronously on the main thread, and stalled the
Wayland event loop on init for any non-trivial repository. This
document is the plan that was executed to rebuild that layer.

The deleted code is preserved at `src/_legacy/{async_polling,app_async_polling}.rs`
as reference (deliberately outside the compile tree — the surrounding
architecture they were written against is gone, so a wholesale restore
wouldn't type-check). `src/watcher.rs` is restored in-tree (it lifted
verbatim — leaf module with `WorktreeInfo` as its only external dep).

## Goals

1. **No synchronous libgit2 work on the main thread, ever.** Tab open,
   refresh, status query, dirty checks, even `Repository::open` — all
   off-thread. The main thread folds results back when ready.
2. **Auto-refresh on external changes** via filesystem watching, with
   priority-tiered debounce so a `git commit` shows up in ~150 ms and
   a working-tree edit in ~500 ms.
3. **Large repos with giant submodules don't head-of-line block.** The
   parent's pill / status updates as soon as the parent finishes,
   independent of any 25 K-file submodule's dirty check.
4. ~~**Cleaner lifecycle than the legacy**~~ — *originally a goal,
   deferred.* The proposed `AsyncEngine` consolidation would have
   given us "one place that owns what work is in flight, what's
   queued, what gets dispatched next." On self-review, the
   consolidation was zero-new-behavior structural prettification of
   code that already worked under load this code hasn't seen. We
   shipped the slot-scatter shape from the legacy verbatim. See
   "What was deferred (and why)" below.

## Non-goals (this round)

- Mid-call cancellation. Closing a tab during a refresh drops the
  result correctly via `tab_id` gating; the worker still finishes.
  Adding `Arc<AtomicBool>` cancellation for coarse-grained early-exit
  is plausible later but git2 ops aren't cancellable mid-call so
  savings are small. Defer until measured.
- Cross-platform watcher tuning. Use `notify` defaults; the 3-tier
  debounce in `RepoWatcher` papers over per-platform event rate.
- Streaming partial results from the commit walk. Today the worker
  runs to completion before sending. Streaming would let the graph
  paint the first 200 commits while the rest load — useful for huge
  histories — but adds enough complexity that we should ship the
  monolithic version first and measure.
- Cache warming / speculative prefetch.

## What the legacy got right (and we lift verbatim)

These are the items the redesign **must** preserve. Each one is a
fix for a specific failure mode we already paid for; deleting any one
is re-buying the bug.

### 1. Three-tier event classifier (`src/watcher.rs`)

`FsChangeKind`:
- `WorkingTree` (priority 0, 500 ms debounce) — file edits outside `.git`
- `GitMetadata` (priority 1, 150 ms debounce) — HEAD/refs/index/packed-refs/etc.
- `WorktreeStructure` (priority 2, 150 ms debounce) — `worktrees/` add/remove

Hard cap of 2 s prevents indefinite deferral under sustained activity.
Higher priority wins when coalescing within a debounce window. The
metadata lane fires faster because git ops feel snappier when the UI
catches up in <200 ms; working-tree edits can wait 500 ms because
they're typed character-by-character anyway.

### 2. Two-tier *spawn* (not just two-tier debounce)

`spawn_status_refresh` is cheap (working-dir status only). `spawn_repo_state_refresh`
is heavy (full commit walk + branches + tags + worktrees + remotes +
ahead/behind + per-worktree GitRepo handles). They are *different
functions* with different costs, dispatched by `FsChangeKind`:

- `WorkingTree` → `spawn_status_refresh` only
- `GitMetadata` → `repo.reopen()` then `spawn_repo_state_refresh`
- `WorktreeStructure` → same as `GitMetadata` plus update watcher paths

Critically, file edits **never trigger a commit walk.** This is the
single biggest cost reduction over a naive "always full refresh."

### 3. Per-entity dirty-check fanout

One worker thread per submodule, one per worktree. Comment from the
legacy explicitly cites esp-idf with 25 K files: "a slow submodule
doesn't block fast ones." Each settles independently; the parent's
pill updates the moment its own check returns, even while a giant
submodule is still scanning.

Each per-entity worker uses `git2::StatusOptions::exclude_submodules(true)`,
so a submodule's dirty check doesn't recurse into its *own* sub-submodules.

### 4. Layered submodule exclusion

The parent's status query, the watcher's classifier, the per-entity
dirty checks — three independent layers all exclude submodules from
parent-scope walks. Removing any one of them brings the parent to its
knees on a repo with giant submodules. We keep all three.

### 5. `tab_id` stale-result rejection

Worker results carry the `tab_id` they were spawned for. When a result
lands, it's matched against the live tab list. Tab closed mid-flight?
Result silently dropped. Tab closed-and-reopened to the same path?
Different `tab_id`, old result still dropped. This is correctness, not
optimization — without it, async results landing during a tab swap
corrupt the wrong tab's state.

### 6. Async watcher init

Even *creating* the inotify watcher is off-thread. Walking the watch
path set on a giant repo (initial recursive watch of the workdir) can
itself take long enough to stall the Wayland handle. The watcher
construction goes on a worker; the result lands in `watcher_init_receiver`.

### 7. `repo.reopen()` cache bypass

libgit2 caches refs at the C level. After an external `git commit`,
the watcher fires correctly, but the next refresh would re-read the
cached HEAD OID — invisible to libgit2. Calling `Repository::open`
on the same path returns a fresh handle that bypasses the cache.
Done before every state refresh triggered by `GitMetadata`/`WorktreeStructure`.
Also done for cached worktree GitRepo handles.

### 8. `ref_fingerprint` reconciliation

Cheap hash of the contents of `git_dir/refs/`, computed every 5 s on
the main thread. If it diverges from the last seen fingerprint
unexpectedly (watcher missed an event, queue overflow, libgit2 cached
state went stale somehow) → reopen + force a full refresh. Belt-and-
braces against watcher gaps.

### 9. 30 s status safety net

Even without a watcher event, the working-dir status is re-queried
at least every 30 s. Catches anything the watcher missed plus any
clock skew or filesystem oddity.

### 10. Stale-data guards in apply step

```
if result.commits.is_empty() && !repo_tab.commits.is_empty() {
    return;  // preserve what we had — don't blank the graph
}
```

Plus: cache previous diff stats and re-apply them after replacing
`tab.commits`, so the +N/-M chips don't flicker during a refresh.
Both are subtle UX rules baked into the reducer; both lift verbatim.

### 11. WHISPER_FRAME_DIAG

Env-var-driven timing breakdown of the apply step (printed to stderr).
Built-in observability for refresh cost; cheap to keep.

## What was proposed as cleaner (and deferred)

The sections below sketch a per-tab `AsyncEngine` consolidation that
was on the table during design and **deferred during self-review**.
Captured here as a record of the proposal — and so a future
contributor with a specific friction case has a head start. The
deferral rationale is in the next section ("What was deferred (and
why)").

### Scattered lifecycle bookkeeping

Legacy: `TabViewState` carried `status_receiver`, `repo_state_receiver`,
`watcher_init_receiver`, `watcher`, `watcher_rx`, `diff_stats_receiver`,
plus parallel `_in_flight` flags scattered across `App`. Each spawn
site checked its own slot, set its own flag, and the apply step had
to know which downstream effects to trigger.

Redesign: a per-tab `AsyncEngine` owns all of it.

```rust
pub struct AsyncEngine {
    tab_id: u64,
    state_slot: AsyncSlot<RepoStateResult>,
    status_slot: AsyncSlot<StatusResult>,
    watcher_init: AsyncSlot<Result<(RepoWatcher, Receiver<FsChangeKind>)>>,
    watcher: Option<RepoWatcher>,
    watcher_rx: Option<Receiver<FsChangeKind>>,
    dirty_checks_in_flight: usize,
    last_status_refresh: Instant,
    last_ref_check: Instant,
    last_ref_fingerprint: u64,
    status_dirty: bool,
    state_dirty: Option<DirtyReason>,
}

struct AsyncSlot<T> {
    rx: Option<Receiver<T>>,
    started_at: Option<Instant>,
}
```

### Pure reducers + effect coordination separated

Legacy `apply_repo_state_result` was a 200-line function that mixed:
1. Folding the `RepoStateResult` into `RepoTab`/`TabViewState` fields
2. Updating watcher paths
3. Spawning the next round of diff stats
4. Spawning per-entity dirty checks
5. Reading + writing `TabViewState` cross-fields (worktree cache pruning, branch sidebar update, etc.)

Redesign: split into `apply_state_result(tab, result) -> ApplyEffects`,
where `ApplyEffects` is a value type:

```rust
pub struct StateApplyEffects {
    pub diff_stats_for: Vec<Oid>,
    pub dirty_checks_submodules: Vec<SubmoduleInfo>,
    pub dirty_checks_worktrees: Vec<WorktreeInfo>,
    pub watcher_paths_changed: bool,
}
```

The reducer is pure: it only mutates `tab` and returns the effects
it produced. The engine reads the effects and dispatches them via
spawn helpers. Side effects move from "buried inside a 200-line
function" to "one match arm in `engine.poll()`."

### Single dirty-marker pipeline

Legacy had three signal sources (watcher, 30 s timer, 5 s ref_check)
each calling `refresh_status_for_tab` / `trigger_repo_state_refresh_for_tab`
directly. Redesign: all three set `status_dirty` / `state_dirty` flags;
the engine's `poll` is the single deduplication point that decides
whether to spawn. Loses no information — `state_dirty: Option<DirtyReason>`
preserves the cause for logging — but consolidates the spawn decision.

### Engine entry points

Three:
- `mark_status_dirty(reason)` — set the bit, don't spawn
- `mark_state_dirty(reason)` — set the bit, don't spawn
- `poll(&mut tab, &proxy, &mut dirty_check_tx)` — once per frame: drain
  results, apply via reducers, dispatch effects, re-spawn from dirty bits

This is the only API surface. Watcher events, cadence timers, and
post-op completion all funnel through the two `mark_*` calls.

## What was deferred (and why)

An earlier draft of this plan proposed an `AsyncEngine` per-tab
consolidation: one struct owning all four async slots, the watcher,
the cadence timers, and the dirty markers, with pure reducers feeding
an effect-dispatcher orchestrator. Cleaner shape than the legacy's
slot scatter on paper. **Deferred during self-review** for these
reasons, all of which still hold:

1. **The legacy worked. New code has zero production miles.** The
   slot-scatter is ugly but every call site is self-contained — you
   can trace a bug by reading one function. An AsyncEngine
   abstraction layer has to be correct end-to-end, and getting it
   wrong means *every* refresh path breaks, not just one.
2. **`ApplyEffects` is a leaky abstraction if not careful.** If the
   reducer needs to read engine state to decide an effect (e.g.
   "spawn dirty checks only if not already in flight"), the boundary
   slips: either pass the engine into the reducer or duplicate state
   in the dispatcher. The legacy avoided this by just having the
   apply step call the spawn helpers directly.
3. **Single dirty-bit deduplication can lose information.** If a
   watcher event says `WorktreeStructure` and a 5 s ref_check says
   "fingerprint diverged," collapsing both into a single
   `state_dirty: Some(_)` loses the "I also need to update watcher
   paths" signal that `WorktreeStructure` carried. Keeping them
   disjoint with separate marks (or a richer `DirtyReason` enum)
   costs complexity. The legacy's direct dispatch per kind is
   structurally simpler — it just looks uglier.
4. **The "improvements" added zero new behavior.** Reviewing the
   AsyncEngine proposal minus the verbatim items, the actually-new
   behavior was *none.* Pure structural rearrangement of code that
   was already correct. The legacy worked under load this new code
   hasn't seen; speculative tidy-up is the wrong place to spend a
   correctness budget.

The right sequencing was port-then-refactor. We did the port; we
deferred the refactor. Future contributors: revisit AsyncEngine only
if a specific structural friction makes the case ("we need a fifth
async slot and the scatter is obstructing it"). "It's prettier"
isn't strong enough.

## What shipped

Six commits, each independently committable, none changing behavior
beyond what the verbatim-from-legacy items already specified:

| # | Commit | Step |
|---|--------|------|
| 1 | `653d270` | Restore async/watcher infrastructure deleted in Phase 7a |
| 2 | `7edca35` | `git_async`: spawn helpers for status, repo-state, dirty-check refresh |
| 3 | `deb3f8a` | async-engine: tab id, refresh slots, reducers, drain plumbing |
| 4 | `78f2fa3` | async-engine: async-init `RepoTab::open` + post-op refresh on worker |
| 5 | `388ab2a` | async-engine: wire watcher consumer with kind-dispatched refresh |
| 6 | `d7bee1a` | async-engine: 30 s status safety net + 5 s ref-fingerprint reconciliation |

End-to-end behavior:
- **No sync libgit2 on the main thread.** Tab open is async-init (no
  Wayland stall on startup). Post-op refreshes (commit / fetch / push
  / pull / branch / stage) all spawn workers via
  `tab.request_state_refresh`. Headless paths (dump_bundles, screenshot
  mode) keep sync via explicit `tab.refresh()` calls — they have no
  event loop to drain async results.
- **Filesystem watcher fires kind-dispatched refresh.** `WorkingTree`
  → status only + per-worktree dirty fanout. `GitMetadata` /
  `WorktreeStructure` → `reopen_repo_handles()` + full state refresh.
  Submodule paths excluded at the classifier; per-entity dirty checks
  fan out one worker per submodule (esp-idf head-of-line guard).
  Watcher init itself runs off-thread.
- **Belt-and-braces safety nets.** 30 s status fallback for inotify
  drops; 5 s `ref_fingerprint` reconciliation for libgit2 cache
  divergence.
- **Stale-data guards in reducers.** Empty result on existing data
  preserves what we have; diff-stats restored by oid prevent +N/-M
  flicker; `tab_id` routes per-entity dirty checks back to the right
  tab even after close-then-reopen.

## Decisions made

The open questions from the design phase, with their resolutions:

1. **Module location**: `src/git_async.rs` as a top-level sibling to
   `crate::git` (matches `crate::avatar` pattern; keeps `git/mod.rs`
   focused on synchronous libgit2 wrappers).
2. **Slot organisation**: flat fields on `RepoTab` (`state_refresh_rx`,
   `status_rx`, `watcher_init_rx`, `watcher`, `watcher_rx`,
   `ref_fingerprint`, `state_refresh_attempted`). Matches the existing
   `diff_stats_rx` shape; substruct grouping deferred unless friction
   shows up.
3. **`ref_fingerprint` cadence with no watcher**: kept at uniform 5 s.
   The 30 s status timer covers working-tree visibility; degrading
   gracefully without watcher is acceptable.
4. **Dirty-check channel**: single global `Sender<DirtyCheckResult>`
   with `tab_id` routing — legacy choice, kept.
5. **Submodule drill-down**: each drilled-in level gets its own
   `RepoTab` and its own watcher / async lifecycle (the auto-init in
   `before_build` walks `nav_stack` too).
6. **Error surfacing**: through `self.toasts.push(ToastSpec::error(...))`
   on the existing toast pipeline.

## Known gaps / follow-ups

- **Submodule path exclusion is set at watcher construction only.**
  Same gap the legacy lived with — there's no `update_submodule_paths`
  on `RepoWatcher`. Submodules added during a session aren't re-
  excluded; events inside them surface as `WorkingTree` and trigger
  spurious parent-status walks until the watcher is recreated. Worth
  adding a sibling to `update_worktree_watches`.
- **Mid-call cancellation.** Closing a tab during a refresh drops the
  result correctly via `tab_id` gating, but the worker still runs to
  completion. Adding `Arc<AtomicBool>` cancellation for coarse-grained
  early-exit (between commit-walk batches, before opening each
  worktree GitRepo) is plausible if we measure it mattering. git2 ops
  aren't cancellable mid-call, so savings are bounded.
- **Streaming partial results from the commit walk.** Today the
  worker runs to completion before sending. Streaming would let the
  graph paint the first 200 commits while the rest load — useful for
  huge histories — but adds enough complexity that we shipped the
  monolithic version first.
- **AsyncEngine consolidation refactor.** Deferred per the self-review
  above. Don't pick it up without a specific friction case.
