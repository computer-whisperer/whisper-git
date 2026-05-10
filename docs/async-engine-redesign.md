# Async Engine Redesign

**Status:** Draft — not yet implemented. Reviewing before building.
**Date:** 2026-05-09
**Branch:** `aetna-ui`

## Why this exists

In Phase 7a (commit `4fbfbc1`, May 8 2026) the pre-port async/watcher
modules were deleted as "fossil code on disk for reference." This was
wrong: those modules encoded a design — earned through real iteration
against real failure modes — that the new architecture still needs.
The current `aetna-ui` branch has no filesystem watcher, runs git
queries synchronously on the main thread, and stalls the Wayland event
loop on init for any non-trivial repository. We need to rebuild this
layer; this document is the plan.

The deleted code is preserved at `src/_legacy/{async_polling,app_async_polling}.rs`
as reference. `src/watcher.rs` is restored in-tree (it lifts verbatim).

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
4. **Cleaner lifecycle than the legacy** — one place that owns "what
   work is in flight, what's queued, what gets dispatched next." The
   legacy worked but the bookkeeping was scattered across `TabViewState`
   field-by-field.

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

## What the legacy got rough (and we improve)

These are the parts we redesign. The behavior stays the same; the
shape changes. **None of these are required to ship — they're
"cleanups" that compound to readability.** Each one is independently
defensible to revert if implementation friction shows up.

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

## Self-review

The user explicitly asked for review before building. Pushing on each
piece honestly:

### Risks of the AsyncEngine refactor

1. **The legacy worked. My code has zero production miles.** The slot-
   scatter is ugly but every call site was self-contained and you could
   trace a bug by reading one function. AsyncEngine introduces an
   abstraction layer that has to be correct — and getting it wrong
   means *every* refresh path breaks, not just one.

2. **`ApplyEffects` is a leaky abstraction if I'm not careful.** If
   the reducer needs to read engine state to decide an effect (e.g.
   "spawn dirty checks only if we don't already have one in flight"),
   we're either back to passing the engine into the reducer or the
   effect dispatcher in the engine has to re-derive context. The
   legacy avoided this by just having the apply step call the spawn
   helpers directly.

3. **Single dirty-bit deduplication may be wrong.** If a watcher event
   says `WorktreeStructure` and a 5 s ref_check says "fingerprint
   diverged," collapsing both into `state_dirty: Some(_)` loses the
   "I also need to update watcher paths" signal that `WorktreeStructure`
   carried. Keeping them disjoint with separate marks (or a richer
   `DirtyReason` enum) costs complexity. The legacy's direct dispatch
   per kind was structurally simpler — it just looked uglier.

4. **Engine becomes a god-object.** Watcher init + watcher itself +
   four async slots + cadence timers + dirty markers all in one
   struct. The legacy had this same data scattered across multiple
   structs but each cluster was small. My consolidation makes the
   `AsyncEngine` 200+ lines as a single type.

### What if I just ported verbatim?

Honest answer: probably ships sooner with fewer regressions. The legacy
is a known-good shape. The improvements I'm proposing are opinions
about cleanliness, not bug fixes. **A port-then-refactor sequence
might be the right call:**

- Phase 1: lift `async_polling.rs` and `app_async_polling.rs` against
  the new `RepoTab` shape (which absorbed `TabViewState`). Behavior
  identical to legacy. Ship and verify.
- Phase 2: refactor toward `AsyncEngine` only if a specific limitation
  bites — e.g. "we want to add a fifth async slot and the scatter is
  obstructing it."

The risk of phase 1 only is that we end up with the slot scatter as
permanent technical debt. The risk of going straight to AsyncEngine
is that I introduce a subtle bug while reshaping code I don't fully
understand. Given the legacy was tuned over months and I read it for
~30 minutes, the conservative call is the port.

### What's actually new vs. what's just shape-shifting

Looking at my proposed redesign minus the verbatim items, the actually-
new behavior is: **none.** Everything I'm proposing is a structural
rearrangement of the legacy. There's no new failure mode handled, no
new performance characteristic, no new feature surface. It's a tidy-up
of code that was already correct.

That should make us suspicious. The legacy worked under load that
this new code hasn't seen. The structural improvements are speculative
quality — they might pay off, but they might also introduce bugs I
won't catch until a user with a specific repo shape trips on them.

### My revised recommendation

**Port verbatim first** (against the new `RepoTab` shape, but otherwise
behavior-identical to legacy). Get the watcher firing, get auto-refresh
working, get the Wayland-stall fixed on tab open. Verify against a
large repo with submodules.

**Refactor to AsyncEngine only after the port is shipped and tested,
and only if a specific structural friction makes the case.** "It's
prettier" isn't a strong enough case to risk regressing the iteration-
earned correctness.

Concretely: the staging plan A–H I proposed earlier is right, but the
ordering should be:

1. Port `RepoStateResult` / `StatusResult` / `DirtyCheckResult` types
   and their spawn helpers from `_legacy/async_polling.rs` to
   `src/git_async.rs`. Preserve every detail: tab_id, exclude_submodules,
   per-entity fanout, stale-data guards.
2. Port the consumer pattern from `_legacy/app_async_polling.rs` to
   `WhisperApp::poll_async_ops`. `Option<Receiver<...>>` slots on
   `RepoTab` directly (matches the existing `diff_stats_rx` shape).
3. Convert `RepoTab::open` to async-init (the Wayland fix).
4. Restore `watcher.rs` consumer, all 3 tiers, layered exclusions.
5. Add `ref_fingerprint` reconciliation, 30 s status safety net.
6. Verify against a large repo with submodules. Frame-diag on. Watch
   for missed events, blank-graph regressions, dirty-state staleness.
7. **Only then** consider AsyncEngine consolidation, and only if step
   6 reveals friction the consolidation actually fixes.

## Open questions for review

These are the calls I'm least sure about; flagging them explicitly.

1. **`crate::async_engine` vs. extending `crate::git`.** The async layer
   could live at `src/git_async.rs` (new module, sibling to `git/`)
   or inside `src/git/async.rs` (integrated). The legacy used a top-level
   `async_polling.rs`. My lean: top-level `src/git_async.rs` — keeps
   `git/mod.rs` focused on synchronous wrappers, the async layer is a
   sibling. Same pattern as `crate::avatar`.

2. **`Option<Receiver<...>>` slots on `RepoTab` vs. a wrapper struct.**
   Even without the AsyncEngine consolidation, we could group the
   slots into a `TabAsyncState` substruct on `RepoTab`. Marginal
   readability win, no behavior change. Skip for now?

3. **What happens to `ref_fingerprint` if the user has no watcher?**
   notify can fail to construct on some filesystems (NFS, certain
   sandboxes). The legacy surfaced an error toast and ran without a
   watcher; in that mode `ref_fingerprint` becomes the only auto-
   refresh signal. Do we want to lower the cadence (5 s → 2 s) when
   the watcher is absent? Or keep it uniform? Lean: keep uniform; the
   30 s status timer covers most of the working-tree visibility, and
   users without a watcher are already on a degraded experience.

4. **Per-tab vs. global `dirty_check_tx`.** Legacy used a single global
   `Sender<DirtyCheckResult>` with `tab_id` routing. Per-tab senders
   would be slightly cleaner but break the "one channel drained per
   frame" pattern. Lean: keep global with `tab_id` (legacy's choice).

5. **Submodule drill-down implications.** Each drilled-in level is its
   own `RepoTab` in `nav_stack`. Each gets its own watcher +
   AsyncEngine? Lean: yes. Costs an extra inotify handle per drill-in
   level (rare; usually 1–2 deep) but correctness is straightforward
   — each level is a fully independent repo subject.

6. **Error visibility.** Legacy surfaced errors via `toast_manager.push(...)`.
   Current arch uses `self.toasts.push(ToastSpec::error(...))`. Same
   shape, just different sink. Trivial, flagging for completeness.

## Decision points needed before building

- [ ] Approve "port verbatim first, refactor only if needed" sequencing
- [ ] Confirm `src/git_async.rs` for the async layer
- [ ] Confirm `Option<Receiver<...>>` slots on `RepoTab` directly (no
      wrapper substruct)
- [ ] Confirm uniform 5 s `ref_fingerprint` cadence regardless of
      watcher availability
- [ ] Confirm per-drilled-tab watcher + async lifecycle

Once these are settled, the implementation plan is the staging A–H
above (renumbered 1–6 in the revised recommendation), with each step
landing as its own commit.
