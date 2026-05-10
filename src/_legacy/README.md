Legacy reference modules — pre-port (May 2026), preserved here so the
data-model shape iteration baked into them isn't lost while we rebuild
their replacements against the current architecture.

These files are deliberately outside the compile tree (no `mod`
declaration in `lib.rs`). They are *reference*, not "to-port-incrementally":
the surrounding architecture they were written against is gone (the old
`App` struct, the `(RepoTab, TabViewState)` pair, the `messages/`
dispatch layer), so a wholesale restore would not type-check and we
would be papering over a real architectural shift if we tried.

What's here, why it's worth keeping:

- `async_polling.rs` (871 lines) — `spawn_status_refresh`, the per-
  worktree dirty-check thread spawning, and the periodic-safety-net
  cadence logic. The thread-spawn helpers + result types are mostly
  pure; the App-coupled bits aren't.

- `app_async_polling.rs` (640 lines) — the consumer pattern: per-tab
  `poll_watcher`, max-priority coalescing of FsChangeKind events,
  in-flight gating to avoid stacking refreshes, and the `repo.reopen()`
  cache-bypass call before re-querying after git-metadata changes.
  This is the piece whose *shape* matters most when we write the new
  consumer against `WhisperApp::poll_async_ops`.

The actual `RepoWatcher` itself lives at `src/watcher.rs` (compiled,
since it's leaf-module pure logic and lifts essentially verbatim).

Once the new consumer + status-refresh threading lands and matches the
behavior the legacy here documents, delete this folder. Until then,
read these files alongside the new code; do not import from them.
