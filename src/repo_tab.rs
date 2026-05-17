//! Per-tab repo state.
//!
//! A tab owns the *reference* `GitRepo` plus repo-level metadata
//! (branches, tags, remotes, submodules, stashes, the commit graph).
//! Worktree-scoped state — the staging area, commit-message draft, the
//! file under preview — lives on a per-worktree [`WorktreeView`]; the
//! tab carries a map of those keyed by working-dir path plus the path
//! of the currently selected one.
//!
//! For single-worktree repos there is exactly one `WorktreeView` (over
//! the main working directory). For repos with linked worktrees the
//! map has one entry per worktree; switching the active path swaps the
//! staging well's view and redirects status / commit / diff operations
//! at the worktree's own `GitRepo` handle.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

use std::sync::mpsc::Receiver;
use std::time::Instant;

use winit::event_loop::EventLoopProxy;

use crate::ci::{CiFetchResult, ProviderCiResult, ProviderCommitRollup};
use crate::commit_graph::GraphLayout;
use crate::config::Config;
use crate::git::{
    BranchTip, CommitInfo, CommitSubmoduleEntry, DiffFile, FullCommitInfo, GitRepo, RemoteOpResult,
    StashEntry, SubmoduleInfo, TagInfo, WorkingDirStatus, WorktreeInfo, insert_synthetics_sorted,
};
use crate::git_async::{
    DirtyCheckResult, RepoStateResult, StatusResult, spawn_repo_state_refresh, spawn_status_refresh,
};
use crate::watcher::{FsChangeKind, RepoWatcher, WatcherInitResult};
use crate::{github, gitlab, token_store};

/// Unique id allocator for [`RepoTab`]. Used by per-entity dirty-check
/// results (which flow over a global channel) to route back to the
/// originating tab — so a closed-then-reopened tab doesn't accidentally
/// receive a stale result intended for the old tab. Starts at 1 because
/// 0 is reserved as a sentinel for "fixture / dump_bundles" tabs that
/// were never meant to spawn workers.
static NEXT_TAB_ID: AtomicU64 = AtomicU64::new(1);

fn next_tab_id() -> u64 {
    NEXT_TAB_ID.fetch_add(1, Ordering::Relaxed)
}

/// In-flight async git op — receiver for the worker-thread result plus
/// metadata for toast / error wording. Carried per-tab per-op-kind so
/// only one of each (fetch / pull / push / mutation) runs at a time
/// for a given repo.
pub struct TimedOp {
    pub rx: Receiver<RemoteOpResult>,
    pub started: Instant,
    /// Human-readable label baked into the success toast / error
    /// summary: `"origin"`, `"main → origin/main"`, `"abc1234"`, etc.
    pub label: String,
}

impl TimedOp {
    pub fn new(rx: Receiver<RemoteOpResult>, label: impl Into<String>) -> Self {
        Self {
            rx,
            started: Instant::now(),
            label: label.into(),
        }
    }
}

/// In-flight AI commit-message generation. Lives in its own slot
/// (rather than reusing `mutation_op`) because the result type
/// differs — `Result<AiResponse, String>` vs `RemoteOpResult` — and
/// because AI generation doesn't conflict with cherry-pick / revert.
pub struct AiOp {
    pub rx: Receiver<Result<crate::ai::AiResponse, String>>,
    pub started: Instant,
    /// The worktree whose `commit_subject`/`commit_body` the result
    /// should land in. Resolved against `worktree_views` at apply
    /// time — if the worktree has gone away (e.g. removed via
    /// `git worktree remove` while the worker was running), the
    /// result is dropped with a toast rather than silently overwriting
    /// some other worktree's draft.
    pub target_path: PathBuf,
}

/// Cached detail for the currently selected commit. Loaded once per
/// selection change so the History details pane doesn't hit libgit2 on
/// every frame.
pub struct CommitDetail {
    pub info: FullCommitInfo,
    pub files: Vec<DiffFile>,
    /// Submodules pinned at this commit, with `changed = true` for any
    /// pin that drifted from the first parent. The list is pre-sorted
    /// changed-first by `submodules_at_commit` so callers can render
    /// straight through.
    pub submodule_entries: Vec<CommitSubmoduleEntry>,
}

/// Cap for `commit_graph()` — first cut, no infinite-scroll. Plenty for
/// the visible viewport even on big repos. Lifted later if needed.
const COMMIT_LIMIT: usize = 1000;

/// Cap for the per-tab diff-stats prefetch. We cover the entire
/// commit list (matching `COMMIT_LIMIT`); the worker emits results
/// in chunks so the UI fills in progressively rather than waiting
/// for the whole backfill before showing anything.
const DIFF_STATS_FETCH_LIMIT: usize = COMMIT_LIMIT;

/// Collapsible top-level sections of the left sidebar. Worktrees and
/// submodules deliberately don't appear here — both belong to the
/// active worktree, not the repo, and surface through the worktree
/// pill bar (top of the staging well) and the staging-well /
/// commit-detail submodule lists respectively.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum SidebarSection {
    Local,
    Remote,
    Tags,
    Stashes,
}

impl SidebarSection {
    pub fn key(self) -> &'static str {
        match self {
            Self::Local => "LOCAL",
            Self::Remote => "REMOTE",
            Self::Tags => "TAGS",
            Self::Stashes => "STASHES",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::Remote => "Remote",
            Self::Tags => "Tags",
            Self::Stashes => "Stashes",
        }
    }

    pub const ALL: [SidebarSection; 4] = [Self::Local, Self::Remote, Self::Tags, Self::Stashes];
}

/// A logical entry in the sidebar — the keyboard cursor lands on these.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum SidebarSelection {
    Local(String),
    Remote { remote: String, branch: String },
    Tag(String),
    Stash(usize),
}

#[derive(Default)]
pub struct SidebarState {
    pub collapsed: HashSet<SidebarSection>,
    /// Per-remote sub-group collapse state inside the Remote section.
    /// Keyed by remote name (e.g. "origin", "upstream"). Default is
    /// expanded; entries here are remotes the user has collapsed.
    pub collapsed_remotes: HashSet<String>,
    pub selected: Option<SidebarSelection>,
}

impl SidebarState {
    pub fn toggle(&mut self, section: SidebarSection) {
        if !self.collapsed.remove(&section) {
            self.collapsed.insert(section);
        }
    }

    pub fn is_collapsed(&self, section: SidebarSection) -> bool {
        self.collapsed.contains(&section)
    }

    pub fn toggle_remote(&mut self, remote: &str) {
        if !self.collapsed_remotes.remove(remote) {
            self.collapsed_remotes.insert(remote.to_string());
        }
    }

    pub fn is_remote_collapsed(&self, remote: &str) -> bool {
        self.collapsed_remotes.contains(remote)
    }
}

/// Per-worktree state.
///
/// Everything that is logically a property of *which working tree the
/// user is operating on* — the staging area, the commit-message draft,
/// the file under preview, and the per-worktree HEAD / branch — lives
/// here. A tab carries one of these per worktree (linked + main); the
/// active one drives the staging well and diff viewer.
pub struct WorktreeView {
    /// Canonical working-dir path. Also the key into `RepoTab::worktree_views`.
    pub path: PathBuf,
    /// Display name. The repo name for the main worktree, the libgit2
    /// worktree name for linked worktrees.
    pub name: String,
    /// `true` for the repo's main worktree, `false` for `git worktree add`-style
    /// linked worktrees. Drives default-active selection on refresh.
    pub is_main: bool,
    /// Open repo handle scoped to this working directory. All
    /// status / stage / commit / hunk operations route through here.
    pub repo: GitRepo,
    /// Working-dir status (staged / unstaged / untracked / conflicted).
    pub status: WorkingDirStatus,
    /// Branch checked out here, or empty when detached.
    pub current_branch: String,
    /// HEAD OID for this worktree.
    pub head_oid: Option<git2::Oid>,
    /// Submodules registered in *this worktree*. Submodules are a
    /// property of the working tree (they live at paths inside it),
    /// not the repo — so each worktree carries its own list. Refreshed
    /// alongside `status` whenever the active view re-runs.
    pub submodules: Vec<SubmoduleInfo>,
    /// Commit-message subject draft (controlled).
    pub commit_subject: String,
    /// Commit-message body draft (controlled).
    pub commit_body: String,
    /// Currently previewed file in the diff pane (None = no diff selected).
    pub selected_diff_file: Option<String>,
}

impl WorktreeView {
    /// Open the repo at `path` and seed an empty view. Returns `None`
    /// when libgit2 can't open the working directory (worktree pruned,
    /// permissions, etc.) so callers can skip silently.
    fn open(path: PathBuf, name: String, is_main: bool) -> Option<Self> {
        let repo = GitRepo::open(&path).ok()?;
        let mut view = Self {
            path,
            name,
            is_main,
            repo,
            status: WorkingDirStatus::default(),
            current_branch: String::new(),
            head_oid: None,
            submodules: Vec::new(),
            commit_subject: String::new(),
            commit_body: String::new(),
            selected_diff_file: None,
        };
        view.refresh();
        Some(view)
    }

    /// Construct from a pre-opened `GitRepo` handle (the worker already
    /// did the open). Skips the post-construction `refresh()` — the
    /// async path expects the worker's `RepoStateResult` to populate
    /// `current_branch` / `head_oid` / `submodules` separately, and the
    /// next `StatusResult` to populate `status`.
    pub fn with_repo(path: PathBuf, name: String, is_main: bool, repo: GitRepo) -> Self {
        Self {
            path,
            name,
            is_main,
            repo,
            status: WorkingDirStatus::default(),
            current_branch: String::new(),
            head_oid: None,
            submodules: Vec::new(),
            commit_subject: String::new(),
            commit_body: String::new(),
            selected_diff_file: None,
        }
    }

    /// Re-query worktree-scoped state (status + branch + HEAD + submodules).
    pub fn refresh(&mut self) {
        self.status = self.repo.status().unwrap_or_default();
        self.refresh_ref_state();
        self.submodules = self.repo.submodules().unwrap_or_default();
    }

    /// Re-query only ref metadata for this worktree. This deliberately
    /// skips status scans, so repo-state refreshes can keep inactive
    /// worktree HEAD/branch data current without losing cached dirty
    /// status or blocking on large working trees.
    fn refresh_ref_state(&mut self) {
        self.current_branch = self.repo.current_branch().unwrap_or_default();
        self.head_oid = self.repo.head_oid().ok();
    }
}

pub struct RepoTab {
    /// Stable id for this tab instance. Used by per-entity dirty-check
    /// results (which flow over a global channel) to route back here;
    /// see [`crate::git_async::DirtyCheckResult`]. Survives refresh /
    /// re-open of the same path *as a different tab* — closing and
    /// reopening produces a new id, so an in-flight result targeting
    /// the old id is dropped.
    pub id: u64,
    /// Reference repo. The handle the tab was opened with — used for
    /// repo-level metadata (branches, remotes, tags, submodules, stashes,
    /// commit graph). For single-worktree repos this also points at the
    /// main worktree's working directory; the corresponding `WorktreeView`
    /// has its own (separate) handle to keep the abstraction uniform.
    pub repo: GitRepo,
    pub repo_name: String,

    // ---- Repo-level metadata ----
    pub branch_tips: Vec<BranchTip>,
    pub remotes: Vec<String>,
    pub tags: Vec<TagInfo>,
    /// Linked-worktree metadata. The main worktree is *not* in this
    /// list — libgit2 only enumerates linked worktrees here.
    pub worktrees: Vec<WorktreeInfo>,
    pub stashes: Vec<StashEntry>,
    pub sidebar: SidebarState,
    /// Reachable commit history, refreshed alongside repo metadata.
    /// Capped at `COMMIT_LIMIT` until infinite-scroll comes back.
    pub commits: Vec<CommitInfo>,
    /// Lane / color assignment for `commits`. Rebuilt each refresh.
    pub graph_layout: GraphLayout,

    // ---- View state (repo-scoped) ----
    /// Currently selected commit. When `Some`, the right-pane upper
    /// shows commit detail instead of the staging well; the center pane
    /// stays on the graph until the user clicks a file (which sets
    /// [`WorktreeView::selected_diff_file`] and pushes the diff into
    /// the center).
    pub selected_commit: Option<git2::Oid>,
    /// Cached detail for `selected_commit`, refreshed via
    /// [`Self::select_commit`] when the selection changes.
    pub commit_detail: Option<CommitDetail>,

    // ---- Per-worktree state ----
    /// One entry per known worktree (main + linked), keyed by working-dir
    /// path. Drafts and selected-diff carry across refreshes; entries are
    /// only removed when the underlying worktree disappears.
    pub worktree_views: HashMap<PathBuf, WorktreeView>,
    /// Path of the worktree the staging well + diff pane currently
    /// operate on. `None` when the repo has no working tree at all
    /// (effectively bare with zero linked worktrees).
    pub active_worktree: Option<PathBuf>,
    /// Display order for the worktree selector UI — sorted by name,
    /// stable across refreshes so the pill bar doesn't jitter.
    pub worktree_order: Vec<PathBuf>,
    /// `true` while the worktree picker dropdown is open. Only consulted
    /// when the staging-well selector renders in dropdown mode (i.e.
    /// more worktrees than fit the pill bar); ignored otherwise.
    pub worktree_picker_open: bool,

    // ---- Async ops ----
    pub fetch_op: Option<TimedOp>,
    pub pull_op: Option<TimedOp>,
    pub push_op: Option<TimedOp>,
    /// Working-tree mutation ops (cherry-pick, revert). Single slot
    /// shared across kinds since they all conflict with each other.
    pub mutation_op: Option<TimedOp>,
    /// In-flight AI commit-message generation. Independent of the
    /// other slots — generation is read-only against the index, so
    /// it can run alongside e.g. a fetch.
    pub ai_op: Option<AiOp>,

    // ---- CI status ----
    /// Latest results, one per provider. The header bar reads these for
    /// the branch-level summary; the graph rows index `ci_per_commit`
    /// (derived) by SHA for per-commit dots.
    pub ci_results: Vec<ProviderCiResult>,
    /// In-flight CI fetches — one per provider per fetch attempt. Drained
    /// each frame; on Ready, the matching `ci_results` entry is replaced.
    pub ci_receivers: Vec<Receiver<ProviderCiResult>>,
    /// When the last CI fetch was kicked off, regardless of outcome.
    /// Drives the dynamic poll cadence in `WhisperApp::poll_ci_refresh`.
    pub last_ci_fetch: Option<Instant>,
    /// When the most recent successful push completed. Within 5 minutes
    /// the CI poll cadence boosts to 15 s so users see new runs appear
    /// quickly after they push.
    pub last_push_time: Option<Instant>,
    /// Per-commit rollups derived from `ci_results`. Recomputed whenever
    /// a new provider result lands.
    pub ci_per_commit: HashMap<String, Vec<ProviderCommitRollup>>,

    // ---- Async refresh slots ----
    /// `true` once the tab's first state-refresh has been spawned.
    /// Used by the orchestrator's `trigger_initial_state_refreshes`
    /// to gate against re-spawning on every frame after a failed
    /// initial refresh — without this, an empty `commits` plus a
    /// `None` slot would re-fire the spawn every frame in tight loop.
    /// Cleared by nothing — once attempted, subsequent refreshes go
    /// through explicit triggers (post-op, watcher, reconciliation).
    pub state_refresh_attempted: bool,
    /// In-flight full repo-state refresh (commits, branches, tags,
    /// worktrees, remotes, submodules, stashes, ahead/behind, plus
    /// pre-opened per-worktree GitRepo handles). Drained in
    /// `WhisperApp::poll_async_ops`; result folds back via
    /// [`Self::apply_state_result`]. Single in-flight per tab —
    /// trigger sites short-circuit when this is `Some`.
    pub state_refresh_rx: Option<Receiver<RepoStateResult>>,
    /// In-flight working-dir status refresh (cheaper than state
    /// refresh — used for working-tree edits). Drained in
    /// `WhisperApp::poll_async_ops`; result folds back via
    /// [`Self::apply_status_result`].
    pub status_rx: Option<Receiver<StatusResult>>,
    /// A status refresh was requested while one may already be in
    /// flight. `trigger_status_refresh` clears this only when it
    /// actually spawns a worker, so watcher events that arrive during
    /// an older scan are replayed immediately after that scan lands.
    pub status_dirty: bool,
    /// Cheap content hash of `git_dir/refs/`, captured on each
    /// successful state refresh. The 5 s reconciliation timer in
    /// `WhisperApp` compares against this and forces a reopen +
    /// state refresh on divergence — belt-and-braces against missed
    /// watcher events.
    pub ref_fingerprint: u64,

    // ---- Filesystem watcher ----
    /// In-flight watcher init (the `notify` recursive watch can stall
    /// hundreds of ms on a large repo, so construction runs on a
    /// worker). Drained in `WhisperApp::poll_watcher_inits`; on
    /// success populates `watcher` + `watcher_rx`, on failure surfaces
    /// a toast and leaves the slots `None` (the tab runs without
    /// auto-refresh — the 30 s status safety net + 5 s ref reconcile
    /// still cover it).
    pub watcher_init_rx: Option<Receiver<WatcherInitResult>>,
    /// Live filesystem watcher for the tab's repo. `None` until init
    /// succeeds, or permanently `None` if init fails (rare — only
    /// happens on filesystems that don't support inotify-equivalent).
    pub watcher: Option<RepoWatcher>,
    /// Output channel from the watcher's debounce thread. Drained
    /// each frame in `WhisperApp::poll_watcher_events`; max-priority
    /// coalescing collapses bursts of events to a single dispatch.
    pub watcher_rx: Option<Receiver<FsChangeKind>>,

    // ---- Diff stats per commit ----
    /// In-flight diff-stats fetcher (one-shot, drained on completion).
    /// Populated by `trigger_diff_stats_fetch`; on Ready we apply
    /// `(insertions, deletions)` to each `commits[i]` entry by Oid.
    pub diff_stats_rx: Option<Receiver<Vec<(git2::Oid, usize, usize)>>>,
    /// Marks whether diff-stats have been fetched for the current
    /// commit list. Cleared on `refresh()` so a fresh load re-fetches.
    pub diff_stats_fetched: bool,

    // ---- History search ----
    /// Query string for the history-view filter. Empty means "no
    /// filter active"; non-empty dims rows whose subject / author /
    /// short-id don't match. Lower-case substring match — same shape
    /// the pre-port used.
    pub search_query: String,
    /// Whether the history-pane search bar is visible. Hidden by
    /// default; Ctrl+F opens it, Escape closes it (and clears the
    /// query). The query persists across tab switches but the bar
    /// visibility is per-tab.
    pub history_search_open: bool,

    // ---- Submodule drill-down ----
    /// Stack of drilled-in submodule views. Each entry is a fully
    /// constructed `RepoTab` opened against the parent's working
    /// directory; the deepest entry is the user's current view. Empty
    /// at root. Nested submodules push further entries onto the same
    /// outermost stack — there's only ever one stack per opened tab.
    pub nav_stack: Vec<RepoTab>,
    /// OID the *parent* worktree pins this RepoTab to (the SHA stored
    /// in the parent's index/tree as the submodule's expected commit).
    /// Set when this RepoTab was pushed onto a nav_stack via
    /// [`Self::enter_submodule`]; `None` at root since no one pins
    /// the user's outermost repo. Drives the "PINNED" pill on the
    /// commit-graph row that matches.
    pub pinned_oid: Option<git2::Oid>,
    /// Path of this submodule *relative to its parent's worktree*,
    /// as recorded in the parent's `.gitmodules`. Set alongside
    /// `pinned_oid` on enter_submodule. Drives stage_submodule_update
    /// (the parent stages by this path) and the post-commit
    /// coordination dialog's labelling.
    pub pinned_path: Option<String>,
}

/// Side-effects produced by [`RepoTab::apply_state_result`]. The reducer
/// is pure (only mutates the tab); this value tells the orchestration
/// layer what downstream work to schedule. Keeping the spawn decisions
/// in the orchestrator (`WhisperApp::poll_async_ops`) instead of buried
/// in the reducer keeps reducer testing simple and the orchestration
/// auditable.
#[derive(Default)]
pub struct StateApplyEffects {
    /// OIDs whose diff stats should be (re-)computed. The orchestrator
    /// hands these to `compute_diff_stats_async`.
    pub diff_stats_for: Vec<git2::Oid>,
    /// Submodules to dirty-check, fanned out one worker per entry.
    pub dirty_checks_submodules: Vec<SubmoduleInfo>,
    /// Worktrees to dirty-check, fanned out one worker per entry.
    pub dirty_checks_worktree_paths: Vec<PathBuf>,
    /// `true` if the resolved worktree set differs from the previous
    /// one; the watcher needs `update_worktree_watches` to add/drop
    /// per-worktree watch paths.
    pub watcher_paths_changed: bool,
    /// Errors collected during the worker run. Surface as toasts;
    /// already non-fatal (the tab still has data thanks to stale-data
    /// guards in the reducer).
    pub errors: Vec<String>,
}

impl RepoTab {
    /// Open the libgit2 handle and return a minimal empty tab. **Does
    /// not refresh** — the heavy work (commit walk, branch listing,
    /// per-worktree GitRepo opens) runs on a worker via
    /// [`Self::trigger_state_refresh`]. Synchronous refresh on tab open
    /// stalled the Wayland event handle on large repos in the pre-port;
    /// the new shape returns immediately with empty data and renders
    /// loading-state placeholders until the worker's result lands.
    ///
    /// Headless callers (dump_bundles, screenshot mode) that have no
    /// event loop and need populated data immediately should call
    /// [`Self::refresh`] right after `open` to do the work synchronously.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let repo = GitRepo::open(&path).context("open repository")?;
        let tab = Self {
            id: next_tab_id(),
            repo_name: repo.repo_name(),
            branch_tips: Vec::new(),
            remotes: Vec::new(),
            tags: Vec::new(),
            worktrees: Vec::new(),
            stashes: Vec::new(),
            sidebar: SidebarState::default(),
            commits: Vec::new(),
            graph_layout: GraphLayout::new(),
            selected_commit: None,
            commit_detail: None,
            worktree_views: HashMap::new(),
            active_worktree: None,
            worktree_order: Vec::new(),
            worktree_picker_open: false,
            fetch_op: None,
            pull_op: None,
            push_op: None,
            mutation_op: None,
            ai_op: None,
            ci_results: Vec::new(),
            ci_receivers: Vec::new(),
            last_ci_fetch: None,
            last_push_time: None,
            ci_per_commit: HashMap::new(),
            state_refresh_attempted: false,
            state_refresh_rx: None,
            status_rx: None,
            status_dirty: false,
            ref_fingerprint: 0,
            watcher_init_rx: None,
            watcher: None,
            watcher_rx: None,
            diff_stats_rx: None,
            diff_stats_fetched: false,
            search_query: String::new(),
            history_search_open: false,
            nav_stack: Vec::new(),
            pinned_oid: None,
            pinned_path: None,
            repo,
        };
        Ok(tab)
    }

    /// Re-query everything from the underlying repo synchronously.
    /// Used by headless callers (dump_bundles, screenshot mode) that
    /// have no event loop to drive the async path. Sets
    /// `state_refresh_attempted` so the orchestrator's auto-init
    /// loop doesn't pile a redundant worker on top of the result we
    /// just produced inline.
    pub fn refresh(&mut self) {
        self.refresh_with_orphans(true);
    }

    pub fn refresh_with_orphans(&mut self, show_orphaned_commits: bool) {
        self.state_refresh_attempted = true;
        self.branch_tips = self.repo.branch_tips().unwrap_or_default();
        self.remotes = self.repo.remote_names();
        self.tags = self.repo.tags().unwrap_or_default();
        self.worktrees = self.repo.worktrees().unwrap_or_default();
        self.stashes = self.repo.stash_list();
        // Commit list might change — let the polling loop re-trigger
        // the diff-stats fetch on the next pass.
        self.diff_stats_fetched = false;
        self.diff_stats_rx = None;
        // Pull orphan commits from reflogs alongside the topo walk so
        // unreachable work — finished rebases, dropped branches —
        // doesn't disappear. Falls back to plain commit_graph on error
        // so a flaky reflog doesn't blank the History view.
        self.commits = if show_orphaned_commits {
            self.repo
                .commit_graph_with_orphans(COMMIT_LIMIT)
                .or_else(|_| self.repo.commit_graph(COMMIT_LIMIT))
                .unwrap_or_default()
        } else {
            self.repo.commit_graph(COMMIT_LIMIT).unwrap_or_default()
        };

        self.rebuild_worktree_views();

        // Refresh the active worktree's status / branch / HEAD. Inactive
        // views keep their cached state — they get refreshed on switch
        // or when the user explicitly fans out (matching the old
        // worktree-selector behavior).
        if let Some(view) = self.active_view_mut() {
            view.refresh();
        }

        // Inject synthetic "uncommitted changes" rows for each dirty
        // worktree, sorted into the commit list by their newest-mtime
        // timestamp so they sit chronologically with their parent. This
        // is what lets the History view show in-progress work alongside
        // committed history.
        let synthetics = self.build_synthetic_entries();
        if !synthetics.is_empty() {
            insert_synthetics_sorted(&mut self.commits, synthetics);
        }
        self.graph_layout.build(&self.commits);

        if let Some(oid) = self.selected_commit
            && !self.commits.iter().any(|c| c.id == oid)
        {
            self.selected_commit = None;
            self.commit_detail = None;
        } else if let Some(oid) = self.selected_commit {
            self.load_commit_detail(oid);
        }

        // Refresh ref_fingerprint so the 5s reconciliation timer has a
        // valid baseline; without this, every reconciliation tick
        // would fire a redundant state refresh because the cached
        // value is 0.
        self.ref_fingerprint = crate::git::ref_fingerprint(self.repo.git_dir());

        // Rewrite branch_tips' `is_head` to reflect the active worktree's
        // HEAD rather than the reference repo's HEAD. For multi-worktree
        // repos these can differ; the sidebar uses `current_branch()`
        // directly, but other consumers iterating `branch_tips` see the
        // worktree-scoped truth.
        let current = self.current_branch().to_string();
        for tip in &mut self.branch_tips {
            tip.is_head = !tip.is_remote && tip.name == current;
        }
    }

    // ========================================================================
    // Async-refresh triggers
    // ========================================================================

    /// Spawn a full repo-state refresh on a worker thread. Idempotent
    /// while a refresh is already in flight (`state_refresh_rx` is
    /// `Some`). Sets `state_refresh_attempted` so the orchestrator's
    /// auto-init loop doesn't re-fire on every frame after a failed
    /// initial spawn.
    pub fn trigger_state_refresh(
        &mut self,
        proxy: &EventLoopProxy<()>,
        show_orphaned_commits: bool,
    ) {
        if self.state_refresh_rx.is_some() {
            return;
        }
        let repo_context_path = self
            .repo
            .workdir()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.repo.git_dir().to_path_buf());
        let staging_context_path = self
            .active_view()
            .and_then(|v| v.repo.workdir().map(|p| p.to_path_buf()))
            .or_else(|| Some(repo_context_path.clone()));
        self.state_refresh_rx = Some(spawn_repo_state_refresh(
            repo_context_path,
            staging_context_path,
            show_orphaned_commits,
            proxy.clone(),
        ));
        self.state_refresh_attempted = true;
    }

    /// Spawn the filesystem watcher for this tab's repo on a worker.
    /// Idempotent — short-circuits when init is in flight or already
    /// completed. The submodule path list comes from the active view's
    /// submodules, which is why callers should run this *after* the
    /// first state refresh has landed (so submodule exclusions are in
    /// place from the watcher's first frame). Calling it sooner is
    /// safe; the exclusion list will just be empty initially and
    /// events inside submodules will surface as `WorkingTree` until
    /// the next refresh updates the watch set.
    pub fn trigger_watcher_init(&mut self, proxy: &EventLoopProxy<()>) {
        if self.watcher.is_some() || self.watcher_init_rx.is_some() {
            return;
        }
        if self.repo.workdir().is_none() && self.worktrees.is_empty() {
            // Bare repo with no workdir or linked worktrees — nothing
            // to watch.
            return;
        }
        let workdir = self.repo.workdir().map(|p| p.to_path_buf());
        let git_dir = self.repo.git_dir().to_path_buf();
        let common_dir = self.repo.common_dir().to_path_buf();
        let worktrees = self.worktrees.clone();
        // Submodule exclusion list: every submodule's absolute workdir
        // path. Events under these dirs are silently dropped by the
        // classifier — submodule dirty state is checked independently.
        let submodule_paths: Vec<PathBuf> = self
            .active_view()
            .map(|v| {
                v.submodules
                    .iter()
                    .map(|sm| v.path.join(&sm.path))
                    .collect()
            })
            .unwrap_or_default();
        self.watcher_init_rx = Some(crate::watcher::spawn_init(
            workdir,
            git_dir,
            common_dir,
            worktrees,
            submodule_paths,
            proxy.clone(),
        ));
    }

    /// Reopen the tab's `GitRepo` plus every cached worktree-view
    /// `GitRepo`. libgit2's refdb caches HEAD/refs at the C level, so
    /// after an external `git commit` (caught by the watcher's
    /// `GitMetadata` event) the next refresh would still see the old
    /// HEAD — calling `Repository::open` on the same path returns a
    /// fresh handle that bypasses the cache. Best-effort: failures are
    /// silent (the next state refresh will surface them).
    pub fn reopen_repo_handles(&mut self) {
        let _ = self.repo.reopen();
        for view in self.worktree_views.values_mut() {
            let _ = view.repo.reopen();
        }
    }

    /// Trigger a state refresh, picking async (worker spawn) when a
    /// proxy is available and falling back to synchronous on-thread
    /// refresh otherwise. The fallback exists for headless callers
    /// (dump_bundles, screenshot mode) that have no event loop to wake
    /// when the worker finishes — interactive callers always have a
    /// proxy and stay off the main thread.
    pub fn request_state_refresh(
        &mut self,
        proxy: Option<&EventLoopProxy<()>>,
        show_orphaned_commits: bool,
    ) {
        match proxy {
            Some(p) => self.trigger_state_refresh(p, show_orphaned_commits),
            None => self.refresh_with_orphans(show_orphaned_commits),
        }
    }

    /// Spawn a working-dir status refresh on a worker thread. Cheap
    /// path used for working-tree-edit watcher events and the 30 s
    /// safety net. Idempotent while a status refresh is in flight.
    pub fn trigger_status_refresh(&mut self, proxy: &EventLoopProxy<()>) {
        if self.status_rx.is_some() {
            self.status_dirty = true;
            return;
        }
        self.status_dirty = false;
        let repo_context_path = self
            .repo
            .workdir()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.repo.git_dir().to_path_buf());
        let staging_context_path = self
            .active_view()
            .and_then(|v| v.repo.workdir().map(|p| p.to_path_buf()))
            .or_else(|| Some(repo_context_path.clone()));
        let is_bare = self.repo.is_effectively_bare();
        self.status_rx = Some(spawn_status_refresh(
            repo_context_path,
            staging_context_path,
            is_bare,
            proxy.clone(),
        ));
    }

    // ========================================================================
    // Async-refresh reducers
    // ========================================================================
    //
    // Pure folds from a `*Result` value into `RepoTab` state. Side effects
    // the caller needs to schedule (downstream diff_stats fetch, per-entity
    // dirty checks, watcher path rebuilds) are returned as a value type so
    // the orchestration in `WhisperApp::poll_async_ops` stays the place that
    // *decides* what to spawn, not buried in here.

    /// Fold a finished [`RepoStateResult`] into the tab. Stale-data guards
    /// preserve existing data on a partial-failure refresh (don't blank the
    /// graph) and re-apply previous diff-stats by oid (don't flicker the
    /// +N/-M chips during a refresh).
    pub fn apply_state_result(&mut self, result: RepoStateResult) -> StateApplyEffects {
        let frame_diag = std::env::var_os("WHISPER_FRAME_DIAG").is_some();
        let t0 = std::time::Instant::now();

        // Stale-data guard: if the worker came back empty (transient git
        // failure, partial result) and we already have data, keep what we
        // have — the next refresh will get a real result. Without this
        // the graph blanks for a frame on every flaky refresh.
        if result.commits.is_empty() && !self.commits.is_empty() {
            return StateApplyEffects::default();
        }

        // Snapshot prev diff-stats so the +N/-M chips don't disappear while
        // a fresh state lands and the diff_stats fetcher is still in flight.
        let prev_stats: HashMap<git2::Oid, (usize, usize)> = self
            .commits
            .iter()
            .filter(|c| c.insertions > 0 || c.deletions > 0)
            .map(|c| (c.id, (c.insertions, c.deletions)))
            .collect();

        let mut commits = result.commits;
        for c in commits.iter_mut() {
            if let Some(&(ins, del)) = prev_stats.get(&c.id) {
                c.insertions = ins;
                c.deletions = del;
            }
        }
        self.commits = commits;
        self.branch_tips = result.branch_tips;
        self.tags = result.tags;
        self.worktrees = result.worktrees.clone();
        self.remotes = result.remote_names;
        self.stashes = result.stashes;
        self.ref_fingerprint = result.ref_fingerprint;
        // Clear cached diff-stats marker so the polling loop re-runs the
        // fetch against the new commit set.
        self.diff_stats_fetched = false;
        self.diff_stats_rx = None;

        // Merge pre-opened worktree GitRepo handles into worktree_views.
        // Existing entries keep their drafts (commit_subject, commit_body,
        // selected_diff_file) and just swap their repo handle; new
        // entries get default empty drafts. Stale entries (worktree
        // pruned from disk) are dropped.
        let watcher_paths_changed = self.merge_worktree_views(result.worktree_repos);

        // Active view's submodules / current_branch / head_oid come from
        // the worker, but re-query against the *currently active* view's
        // repo handle in case the user switched worktrees mid-spawn.
        let submodules = if let Some(view) = self.active_view() {
            view.repo.submodules().unwrap_or(result.submodules.clone())
        } else {
            result.submodules.clone()
        };
        if let Some(view) = self.active_view_mut() {
            view.submodules = submodules.clone();
            view.current_branch = view
                .repo
                .current_branch()
                .unwrap_or_else(|_| result.current_branch.clone());
            view.head_oid = view.repo.head_oid().ok().or(result.head_oid);
        }

        // Patch branch_tips' is_head against the active worktree's HEAD —
        // matches the sync `refresh()` path.
        let current = self.current_branch().to_string();
        for tip in &mut self.branch_tips {
            tip.is_head = !tip.is_remote && tip.name == current;
        }

        self.rebuild_synthetic_entries();

        // Refresh selected commit detail if the selection's still valid.
        if let Some(oid) = self.selected_commit
            && !self.commits.iter().any(|c| c.id == oid)
        {
            self.selected_commit = None;
            self.commit_detail = None;
        } else if let Some(oid) = self.selected_commit {
            self.load_commit_detail(oid);
        }

        if frame_diag {
            eprintln!(
                "[frame_diag] apply_state_result(tab={}): {} commits, {} worktrees, {:.1}ms",
                self.id,
                self.commits.len(),
                self.worktrees.len(),
                t0.elapsed().as_secs_f64() * 1000.0
            );
        }

        StateApplyEffects {
            diff_stats_for: result.real_oids,
            dirty_checks_submodules: submodules,
            dirty_checks_worktree_paths: self.worktree_order.clone(),
            watcher_paths_changed,
            errors: result.errors,
        }
    }

    /// Fold a finished [`StatusResult`] into the worktree view each
    /// status was captured from, then rebuild synthetic rows from the
    /// same per-worktree cache.
    /// Cheap; called from working-tree-edit paths and from the 30 s
    /// status safety net timer.
    pub fn apply_status_result(&mut self, result: StatusResult) {
        let StatusResult {
            main_path,
            main_status,
            staging_path,
            staging_status,
            staging_repo_state: _,
        } = result;

        let mut changed = false;
        if let (Some(path), Some(status)) = (main_path.as_deref(), main_status) {
            changed |= self.set_worktree_status(path, status);
        }
        if let (Some(path), Some(status)) = (staging_path.as_deref(), staging_status) {
            changed |= self.set_worktree_status(path, status);
        }

        if changed {
            self.rebuild_synthetic_entries();
        }
    }

    /// Fold one finished [`DirtyCheckResult`] into the tab. Returns
    /// `true` when a worktree's cached status changed and synthetic
    /// uncommitted-changes rows were rebuilt.
    pub fn apply_dirty_check_result(&mut self, result: DirtyCheckResult) -> bool {
        match result {
            DirtyCheckResult::Submodule {
                tab_id: _,
                name,
                is_dirty,
            } => {
                if let Some(view) = self.active_view_mut()
                    && let Some(sm) = view.submodules.iter_mut().find(|s| s.name == name)
                {
                    sm.is_dirty = Some(is_dirty);
                }
                false
            }
            DirtyCheckResult::Worktree {
                tab_id: _,
                path,
                status,
            } => {
                let dirty_file_count = status.total_files();
                let is_dirty = dirty_file_count > 0;
                if let Some(wt) = self
                    .worktrees
                    .iter_mut()
                    .find(|w| Path::new(&w.path) == path.as_path())
                {
                    wt.is_dirty = Some(is_dirty);
                    wt.dirty_file_count = Some(dirty_file_count);
                }
                let changed = self.set_worktree_status(&path, status);
                if changed {
                    self.rebuild_synthetic_entries();
                }
                changed
            }
        }
    }

    fn set_worktree_status(&mut self, path: &Path, status: WorkingDirStatus) -> bool {
        let Some(view) = self.worktree_views.get_mut(path) else {
            return false;
        };
        let changed = view.status != status;
        view.status = status;
        changed
    }

    /// Rebuild synthetic "uncommitted changes" rows from the
    /// per-worktree view cache. This deliberately ignores
    /// `self.worktrees`: libgit2's linked-worktree list omits the main
    /// worktree, while `worktree_views` is the UI/data-model set.
    fn rebuild_synthetic_entries(&mut self) {
        self.commits.retain(|c| !c.is_synthetic);
        let synthetics = self.build_synthetic_entries();
        if !synthetics.is_empty() {
            insert_synthetics_sorted(&mut self.commits, synthetics);
        }
        self.graph_layout.build(&self.commits);
    }

    /// Merge worker-pre-opened worktree GitRepo handles into the per-tab
    /// `worktree_views` map. New entries get default drafts; existing
    /// entries keep their drafts and selected-diff but swap to the fresh
    /// repo handle. Returns `true` if the resolved set differs from the
    /// previous one (the watcher needs `update_worktree_watches`).
    fn merge_worktree_views(&mut self, mut pre_opened: HashMap<PathBuf, GitRepo>) -> bool {
        let mut new_views: HashMap<PathBuf, WorktreeView> = HashMap::new();
        let mut order: Vec<(String, PathBuf)> = Vec::new();

        // Main worktree first, so `is_main = true` lands on it.
        if let Some(main_wd) = self.repo.workdir().map(Path::to_path_buf) {
            let name = main_wd
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| self.repo_name.clone());
            let view = if let Some(mut existing) = self.worktree_views.remove(&main_wd) {
                if let Some(repo) = pre_opened.remove(&main_wd) {
                    existing.repo = repo;
                }
                Some(existing)
            } else if let Some(repo) = pre_opened.remove(&main_wd) {
                Some(WorktreeView::with_repo(
                    main_wd.clone(),
                    name.clone(),
                    true,
                    repo,
                ))
            } else {
                // Worker didn't pre-open the main worktree's path (e.g.
                // bare repo with no workdir). Fall back to a sync open;
                // for normal repos the worker covers it.
                WorktreeView::open(main_wd.clone(), name.clone(), true)
            };
            if let Some(mut v) = view {
                v.refresh_ref_state();
                order.push((v.name.clone(), v.path.clone()));
                new_views.insert(main_wd, v);
            }
        }

        // Linked worktrees from the worker's worktrees list.
        for wt in &self.worktrees {
            let path = PathBuf::from(&wt.path);
            if new_views.contains_key(&path) {
                continue;
            }
            let view = if let Some(mut existing) = self.worktree_views.remove(&path) {
                if let Some(repo) = pre_opened.remove(&path) {
                    existing.repo = repo;
                }
                Some(existing)
            } else if let Some(repo) = pre_opened.remove(&path) {
                Some(WorktreeView::with_repo(
                    path.clone(),
                    wt.name.clone(),
                    false,
                    repo,
                ))
            } else {
                WorktreeView::open(path.clone(), wt.name.clone(), false)
            };
            if let Some(mut v) = view {
                v.refresh_ref_state();
                order.push((v.name.clone(), v.path.clone()));
                new_views.insert(path, v);
            }
        }

        // Sort by display name for stable order across refreshes.
        order.sort_by(|a, b| a.0.cmp(&b.0));
        let new_order: Vec<PathBuf> = order.into_iter().map(|(_, p)| p).collect();
        let paths_changed = self.worktree_order != new_order;
        self.worktree_views = new_views;
        self.worktree_order = new_order;

        // Make sure active_worktree still points at a valid entry.
        if let Some(active) = self.active_worktree.clone()
            && !self.worktree_views.contains_key(&active)
        {
            self.active_worktree = self.worktree_order.first().cloned();
        } else if self.active_worktree.is_none() {
            self.active_worktree = self.worktree_order.first().cloned();
        }

        paths_changed
    }

    /// Build a synthetic "uncommitted changes" entry per dirty worktree
    /// view. Each entry carries the worktree's name (for the WT: pill),
    /// HEAD oid as parent, dirty file count, and computed insertion /
    /// deletion stats.
    ///
    /// Unlike the old `git::create_synthetic_entries` (which only knew
    /// about libgit2's linked worktrees and silently skipped the main
    /// worktree in multi-worktree setups), this walks every WorktreeView
    /// the tab tracks — main + linked — so dirty state is never invisible.
    fn build_synthetic_entries(&self) -> Vec<CommitInfo> {
        let mut out = Vec::new();
        for (path, view) in &self.worktree_views {
            let count = view.status.total_files();
            if count == 0 {
                continue;
            }
            let head = match view.head_oid {
                Some(o) => o,
                None => continue,
            };
            let parent_time = self
                .commits
                .iter()
                .find(|c| c.id == head)
                .map(|c| c.time)
                .unwrap_or(0);
            // Build a transient WorktreeInfo so we can reuse the existing
            // `synthetic_for_worktree` constructor (sentinel oid hash, mtime
            // probing). The real `worktrees` field stays libgit2-shape.
            let wt_info = WorktreeInfo {
                name: view.name.clone(),
                path: path.to_string_lossy().to_string(),
                branch: view.current_branch.clone(),
                head_oid: view.head_oid,
                is_dirty: Some(true),
                dirty_file_count: Some(count),
            };
            if let Some(mut entry) = CommitInfo::synthetic_for_worktree(&wt_info, parent_time) {
                let (ins, del) = view.repo.working_tree_diff_stats();
                entry.insertions = ins;
                entry.deletions = del;
                out.push(entry);
            }
        }
        out
    }

    /// Build / prune the worktree-view map. Preserves drafts on existing
    /// entries; opens new ones for newly-appeared worktrees; drops entries
    /// for worktrees that have been pruned.
    fn rebuild_worktree_views(&mut self) {
        let mut new_views: HashMap<PathBuf, WorktreeView> = HashMap::new();
        let mut order: Vec<(String, PathBuf)> = Vec::new();

        // Main worktree (if the reference repo has a working dir at all).
        // Use the directory basename for naming — that's what libgit2 uses
        // for linked worktrees, so the pill labels and sidebar entries
        // agree. (When opened from inside a linked worktree, this path
        // matches one of the linked entries; the dedupe below skips the
        // duplicate insert.)
        if let Some(main_wd) = self.repo.workdir().map(Path::to_path_buf) {
            let entry = self.worktree_views.remove(&main_wd).or_else(|| {
                let name = main_wd
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| self.repo_name.clone());
                WorktreeView::open(main_wd.clone(), name, true)
            });
            if let Some(mut v) = entry {
                v.refresh_ref_state();
                order.push((v.name.clone(), v.path.clone()));
                new_views.insert(main_wd, v);
            }
        }

        // Linked worktrees from libgit2.
        for wt in &self.worktrees {
            let path = PathBuf::from(&wt.path);
            if new_views.contains_key(&path) {
                continue;
            }
            let entry = self
                .worktree_views
                .remove(&path)
                .or_else(|| WorktreeView::open(path.clone(), wt.name.clone(), false));
            if let Some(mut v) = entry {
                v.refresh_ref_state();
                order.push((v.name.clone(), v.path.clone()));
                new_views.insert(path, v);
            }
        }

        order.sort_by(|a, b| a.0.cmp(&b.0));
        self.worktree_order = order.into_iter().map(|(_, p)| p).collect();
        self.worktree_views = new_views;

        // Resolve / repair active worktree selection.
        let still_valid = self
            .active_worktree
            .as_ref()
            .is_some_and(|p| self.worktree_views.contains_key(p));
        if !still_valid {
            // Default: prefer the main worktree, then first in display order.
            let main = self
                .worktree_views
                .iter()
                .find(|(_, v)| v.is_main)
                .map(|(p, _)| p.clone());
            self.active_worktree = main.or_else(|| self.worktree_order.first().cloned());
        }
    }

    /// Switch the active worktree to `path`. Opens (and caches) a
    /// `WorktreeView` for it if not already in the map. No-op when
    /// `path` doesn't resolve to an openable repository.
    pub fn select_worktree(&mut self, path: PathBuf) {
        if !self.worktree_views.contains_key(&path) {
            // Look up display name from the linked-worktree metadata; fall
            // back to the path's basename so we never end up with an empty
            // pill label.
            let path_str = path.to_string_lossy();
            let name = self
                .worktrees
                .iter()
                .find(|w| w.path == path_str)
                .map(|w| w.name.clone())
                .or_else(|| path.file_name().map(|n| n.to_string_lossy().to_string()))
                .unwrap_or_default();
            let is_main = self.repo.workdir().is_some_and(|wd| wd == path.as_path());
            match WorktreeView::open(path.clone(), name, is_main) {
                Some(v) => {
                    self.worktree_views.insert(path.clone(), v);
                    if !self.worktree_order.iter().any(|p| p == &path) {
                        self.worktree_order.push(path.clone());
                    }
                }
                None => return,
            }
        }
        self.active_worktree = Some(path);
        if let Some(v) = self.active_view_mut() {
            v.refresh();
        }
        self.rebuild_synthetic_entries();

        let current = self.current_branch().to_string();
        for tip in &mut self.branch_tips {
            tip.is_head = !tip.is_remote && tip.name == current;
        }
    }

    /// Active worktree view, if any. `None` only for effectively-bare
    /// repos with no linked worktrees.
    pub fn active_view(&self) -> Option<&WorktreeView> {
        self.active_worktree
            .as_ref()
            .and_then(|p| self.worktree_views.get(p))
    }

    pub fn active_view_mut(&mut self) -> Option<&mut WorktreeView> {
        self.active_worktree
            .as_ref()
            .and_then(|p| self.worktree_views.get_mut(p))
    }

    /// Repo handle the active worktree operates on. Falls back to the
    /// reference repo when no worktree is selected, so callers that
    /// only need read-only access (commit graph, full_commit_info,
    /// branch enumeration) don't have to special-case the absent view.
    pub fn active_repo(&self) -> &GitRepo {
        self.active_view().map(|v| &v.repo).unwrap_or(&self.repo)
    }

    /// Branch checked out in the active worktree, or empty when detached
    /// / no worktree selected. Used by the sidebar to highlight the HEAD
    /// branch and by the header bar to display the current branch label.
    pub fn current_branch(&self) -> &str {
        self.active_view()
            .map(|v| v.current_branch.as_str())
            .unwrap_or("")
    }

    /// `true` when there's at least one worktree to render in the
    /// pill bar. Hidden only when the repo has no working tree at all
    /// (effectively bare with zero linked worktrees).
    pub fn has_worktree_selector(&self) -> bool {
        !self.worktree_order.is_empty()
    }

    /// Switch the History view's selected commit. Clears the cached
    /// detail when `oid` is `None`; otherwise loads metadata + diff.
    /// Synthetic rows have sentinel oids that aren't backed by real
    /// commit objects — selecting one would just produce a perpetual
    /// "Loading…" pane, so they're ignored here. The WT pill remains
    /// the affordance for switching to that worktree.
    pub fn select_commit(&mut self, oid: Option<git2::Oid>) {
        let is_synthetic = oid
            .and_then(|o| self.commits.iter().find(|c| c.id == o))
            .map(|c| c.is_synthetic)
            .unwrap_or(false);
        if is_synthetic {
            return;
        }
        if self.selected_commit == oid && self.commit_detail.is_some() {
            return;
        }
        self.selected_commit = oid;
        match oid {
            Some(o) => self.load_commit_detail(o),
            None => self.commit_detail = None,
        }
    }

    fn load_commit_detail(&mut self, oid: git2::Oid) {
        let info = match self.repo.full_commit_info(oid) {
            Ok(i) => i,
            Err(_) => {
                self.commit_detail = None;
                return;
            }
        };
        let files = self.repo.diff_for_commit(oid).unwrap_or_default();
        let submodule_entries = self.repo.submodules_at_commit(oid).unwrap_or_default();
        self.commit_detail = Some(CommitDetail {
            info,
            files,
            submodule_entries,
        });
    }

    /// Local branches sorted alphabetically.
    pub fn local_branches(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .branch_tips
            .iter()
            .filter(|t| !t.is_remote)
            .map(|t| t.name.as_str())
            .collect();
        v.sort_unstable();
        v
    }

    /// Currently focused view — the deepest entry on `nav_stack` if
    /// the user has drilled into a submodule, otherwise self. The whole
    /// renderer + most route handlers consult this rather than `self`
    /// so submodule focus is invisible to widget code: drilled or not,
    /// they get a fully-constructed `RepoTab` to render.
    pub fn active_view_tab(&self) -> &RepoTab {
        match self.nav_stack.last() {
            Some(t) => t,
            None => self,
        }
    }

    /// Mutable counterpart to [`Self::active_view_tab`]. Mirrors the
    /// accessor, but returns `&mut self` when not drilled in (so call
    /// sites don't need a separate "are we focused?" branch to mutate).
    pub fn active_view_tab_mut(&mut self) -> &mut RepoTab {
        if self.nav_stack.is_empty() {
            self
        } else {
            self.nav_stack.last_mut().expect("non-empty checked above")
        }
    }

    /// Immediate parent of the focused view — the entry one above the
    /// stack tip. Returns `self` when drilled in by exactly one level
    /// (the outermost is the parent). `None` at root since root has
    /// no parent above it. Drives the sibling-submodule strip.
    pub fn parent_of_focus(&self) -> Option<&RepoTab> {
        match self.nav_stack.len() {
            0 => None,
            1 => Some(self),
            n => self.nav_stack.get(n - 2),
        }
    }

    /// Drill into a submodule by path (relative to the *currently
    /// focused* worktree). Pushes a freshly-opened `RepoTab` onto the
    /// nav stack — the renderer naturally swaps to it on the next
    /// frame. Errors propagate so the caller can surface a toast.
    ///
    /// Recursive: drilling from inside a submodule pushes another
    /// entry on the same stack, so the chain is `outer › child ›
    /// grandchild` no matter how many levels deep.
    pub fn enter_submodule(&mut self, sm_path: &str) -> Result<()> {
        let active = self.active_view_tab();
        // Resolve against the focused worktree's working directory
        // (not the focused tab's reference repo) so submodules of a
        // linked worktree open at the right path.
        let view = active
            .active_view()
            .context("focused view has no working directory to resolve submodule against")?;
        let abs_path = view.path.join(sm_path);
        // Capture what the parent's HEAD pins this submodule to so
        // the drilled-in graph can highlight the matching commit.
        // Match by path (libgit2 reports paths verbatim; names can be
        // arbitrary).
        let pinned_oid = view
            .submodules
            .iter()
            .find(|s| s.path == sm_path)
            .and_then(|s| s.head_oid);
        let mut new_tab = RepoTab::open(&abs_path)
            .with_context(|| format!("opening submodule at {}", abs_path.display()))?;
        new_tab.pinned_oid = pinned_oid;
        new_tab.pinned_path = Some(sm_path.to_string());
        self.nav_stack.push(new_tab);
        Ok(())
    }

    /// Pop the deepest drilled-in view, returning `true` if anything
    /// was popped. Escape unwinding calls this before falling through
    /// to its other unwind steps so a single Escape can climb one level
    /// at a time.
    pub fn exit_submodule(&mut self) -> bool {
        self.nav_stack.pop().is_some()
    }

    /// Pop until exactly `target_depth` entries remain on the nav stack.
    /// `target_depth = 0` returns to root. Used by breadcrumb clicks.
    pub fn exit_to_depth(&mut self, target_depth: usize) {
        self.nav_stack.truncate(target_depth);
    }

    /// Switch laterally to a sibling submodule of the focused view —
    /// pop the current view off the stack, then drill into `sm_path`
    /// from the parent's perspective. Net effect: stack depth stays
    /// the same. No-op if not drilled in (no parent to drill from).
    pub fn switch_sibling_submodule(&mut self, sm_path: &str) -> Result<()> {
        if self.nav_stack.is_empty() {
            anyhow::bail!("not drilled into a submodule; nothing to switch from");
        }
        self.nav_stack.pop();
        self.enter_submodule(sm_path)
    }

    /// 0 at root, N when the user has drilled in N levels.
    pub fn nav_depth(&self) -> usize {
        self.nav_stack.len()
    }

    /// Names of every level in the navigation chain, outermost first.
    /// Drives the breadcrumb chrome.
    pub fn nav_chain_names(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.nav_stack.len() + 1);
        out.push(self.repo_name.clone());
        for t in &self.nav_stack {
            out.push(t.repo_name.clone());
        }
        out
    }

    /// Kick off CI fetches for every configured remote that maps to a
    /// known provider. One worker thread per (provider, remote) — each
    /// thread sends its result back via `ci_receivers` and wakes the
    /// event loop through `proxy`. `last_ci_fetch` is only stamped when
    /// at least one fetch actually launched: on the first frame after
    /// `RepoTab::open` the async state refresh hasn't populated
    /// `self.remotes` yet, so the loop has nothing to iterate. Stamping
    /// regardless would hide that empty-remotes race behind the 5-min
    /// dynamic interval (CI badges would show 5 minutes late on every
    /// open). Leaving the stamp untouched lets the next poll retry
    /// once the state-refresh worker fills the field.
    ///
    /// Tokens come from the system keychain via `token_store`. GitLab
    /// hosts seen here are also auto-registered into `config.gitlab_hosts`
    /// (and the config persisted) so the token modal has something to
    /// enumerate even before the user has set a token for the host.
    pub fn trigger_ci_fetch(&mut self, config: &mut Config, proxy: EventLoopProxy<()>) {
        if self.remotes.is_empty() {
            // Early-out so the keychain query below doesn't fire 60×/s
            // while we wait for the state-refresh worker to populate
            // `self.remotes`. The `launched`-gated stamp at the bottom
            // would also leave `last_ci_fetch` correctly unset, but the
            // keychain hit isn't free.
            return;
        }
        let github_token = token_store::get_github_token();
        let mut seen_github = false;
        let mut seen_gitlab: HashSet<String> = HashSet::new();
        let mut config_dirty = false;
        let mut launched = false;
        for remote in &self.remotes {
            let url = match self.repo.remote_url(remote) {
                Some(u) => u,
                None => continue,
            };
            if !seen_github
                && let Some(token) = github_token.as_deref()
                && let Some(rx) = github::fetch_ci_status_async(token, &url, proxy.clone())
            {
                self.ci_receivers.push(rx);
                seen_github = true;
                launched = true;
                continue;
            }
            if let Some(parsed) = gitlab::parse_gitlab_remote(&url) {
                if !seen_gitlab.insert(parsed.api_base.clone()) {
                    continue;
                }
                let host = parsed
                    .api_base
                    .strip_prefix("https://")
                    .or_else(|| parsed.api_base.strip_prefix("http://"))
                    .unwrap_or(&parsed.api_base);
                if config.register_gitlab_host(host) {
                    config_dirty = true;
                }
                if let Some(token) = token_store::get_gitlab_token(host)
                    && let Some(rx) = gitlab::fetch_ci_status_async(&token, &url, proxy.clone())
                {
                    self.ci_receivers.push(rx);
                    launched = true;
                }
            }
        }
        if config_dirty {
            // Best-effort save — a write error is non-fatal here (the
            // host stays in memory; next save will pick it up). We
            // intentionally don't surface a toast for this background
            // path since it'd be noise on every CI poll.
            let _ = config.save();
        }
        if launched {
            self.last_ci_fetch = Some(Instant::now());
        }
    }

    /// Synchronous variant of [`Self::trigger_diff_stats_fetch`] —
    /// computes diff stats inline and applies them to `commits`. Used
    /// from the screenshot pipeline (which has no polling loop to
    /// drain the async receiver). Caps at the same fetch limit so
    /// behavior matches the async path.
    pub fn fetch_diff_stats_sync(&mut self) {
        if self.commits.is_empty() {
            return;
        }
        let oids: Vec<git2::Oid> = self
            .commits
            .iter()
            .filter(|c| !c.is_synthetic)
            .take(DIFF_STATS_FETCH_LIMIT)
            .map(|c| c.id)
            .collect();
        let stats = self.repo.compute_diff_stats_sync(&oids);
        let by_oid: HashMap<git2::Oid, (usize, usize)> = stats
            .into_iter()
            .map(|(oid, ins, del)| (oid, (ins, del)))
            .collect();
        for c in &mut self.commits {
            if let Some(&(ins, del)) = by_oid.get(&c.id) {
                c.insertions = ins;
                c.deletions = del;
            }
        }
        self.diff_stats_fetched = true;
    }

    /// Spawn a background fetch of `(insertions, deletions)` for the
    /// most-recent `DIFF_STATS_FETCH_LIMIT` commits in `self.commits`.
    /// One worker per call; the result lands as a single `Vec` on the
    /// receiver and gets folded into `commits` via `drain_diff_stats`.
    /// Idempotent: skips if a fetch is already in flight or has
    /// completed for the current commit list.
    pub fn trigger_diff_stats_fetch(&mut self, proxy: EventLoopProxy<()>) {
        if self.diff_stats_rx.is_some() || self.diff_stats_fetched {
            return;
        }
        if self.commits.is_empty() {
            return;
        }
        // Cap the fetch — backfilling stats for thousands of historical
        // commits is wasteful; users rarely scroll past the first few
        // hundred. Future work: re-trigger as the viewport scrolls past
        // the covered range.
        let oids: Vec<git2::Oid> = self
            .commits
            .iter()
            .filter(|c| !c.is_synthetic)
            .take(DIFF_STATS_FETCH_LIMIT)
            .map(|c| c.id)
            .collect();
        if oids.is_empty() {
            return;
        }
        self.diff_stats_rx = Some(self.repo.compute_diff_stats_async(oids, proxy));
    }

    /// Drain in-flight diff-stats fetches and apply per-commit
    /// `(insertions, deletions)` onto matching `commits` entries.
    /// The worker emits in chunks (see `DIFF_STATS_CHUNK_SIZE`); we
    /// loop until `try_recv` reports `Empty` so all chunks ready this
    /// tick get folded together. The channel only closes when the
    /// worker has finished — that's the trigger for `diff_stats_fetched`,
    /// so re-triggers stay quiet until a `refresh()` clears it.
    /// Returns true if any stats landed (caller can request a redraw).
    pub fn drain_diff_stats(&mut self) -> bool {
        use std::sync::mpsc::TryRecvError;
        let Some(rx) = self.diff_stats_rx.as_ref() else {
            return false;
        };
        let mut any = false;
        loop {
            match rx.try_recv() {
                Ok(results) => {
                    let by_oid: HashMap<git2::Oid, (usize, usize)> = results
                        .into_iter()
                        .map(|(oid, ins, del)| (oid, (ins, del)))
                        .collect();
                    for c in &mut self.commits {
                        if let Some(&(ins, del)) = by_oid.get(&c.id) {
                            c.insertions = ins;
                            c.deletions = del;
                        }
                    }
                    any = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.diff_stats_rx = None;
                    self.diff_stats_fetched = true;
                    break;
                }
            }
        }
        any
    }

    /// Drain in-flight CI receivers. For each Ready result, replace any
    /// existing entry for the same provider, keep the list sorted by
    /// provider, and rebuild `ci_per_commit`. Returns true if any new
    /// result landed (so the caller can request a redraw).
    pub fn drain_ci_receivers(&mut self) -> bool {
        use std::sync::mpsc::TryRecvError;
        let mut changed = false;
        self.ci_receivers.retain(|rx| match rx.try_recv() {
            Ok(result) => {
                self.ci_results.retain(|r| r.provider != result.provider);
                self.ci_results.push(result);
                self.ci_results.sort_by_key(|r| r.provider.sort_key());
                changed = true;
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => false,
        });
        if changed {
            let merged = CiFetchResult {
                providers: self.ci_results.clone(),
            };
            self.ci_per_commit = merged.per_commit_provider_rollups();
        }
        changed
    }

    /// Remote branches grouped by remote name. Within each remote, the
    /// branch list is sorted alphabetically. `origin/HEAD` and similar
    /// symref aliases are filtered out — git2 surfaces them as branches
    /// but they're not meaningful entries in a sidebar.
    pub fn remote_branches(&self) -> Vec<(String, Vec<String>)> {
        let mut by_remote: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for tip in &self.branch_tips {
            if !tip.is_remote {
                continue;
            }
            let (remote, branch) = match tip.name.split_once('/') {
                Some(p) => p,
                None => continue,
            };
            if branch == "HEAD" {
                continue;
            }
            by_remote
                .entry(remote.to_string())
                .or_default()
                .push(branch.to_string());
        }
        for branches in by_remote.values_mut() {
            branches.sort_unstable();
        }
        by_remote.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use anyhow::Result;

    use super::*;

    fn unique_temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "whisper-git-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    fn commit_initial_file(repo: &git2::Repository, path: &Path, contents: &str) -> Result<()> {
        fs::write(repo.workdir().unwrap().join(path), contents)?;
        let mut index = repo.index()?;
        index.add_path(path)?;
        index.write()?;
        let tree_id = index.write_tree()?;
        let tree = repo.find_tree(tree_id)?;
        let sig = git2::Signature::now("Whisper Test", "test@example.com")?;
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])?;
        Ok(())
    }

    fn repo_state_result(repo: &GitRepo) -> Result<RepoStateResult> {
        let commits = repo.commit_graph(COMMIT_LIMIT)?;
        let real_oids = commits.iter().map(|c| c.id).collect();
        let worktrees = repo.worktrees()?;
        let worktree_repos = worktrees
            .iter()
            .filter_map(|wt| {
                let path = PathBuf::from(&wt.path);
                GitRepo::open(&path).ok().map(|repo| (path, repo))
            })
            .collect();
        Ok(RepoStateResult {
            commits,
            branch_tips: repo.branch_tips()?,
            tags: repo.tags().unwrap_or_default(),
            current_branch: repo.current_branch().unwrap_or_default(),
            head_oid: repo.head_oid().ok(),
            worktrees,
            remote_names: repo.remote_names(),
            remote_urls: HashMap::new(),
            is_bare: repo.is_effectively_bare(),
            submodules: repo.submodules().unwrap_or_default(),
            stashes: repo.stash_list(),
            ahead_behind: repo.all_branches_ahead_behind(),
            ref_fingerprint: crate::git::ref_fingerprint(repo.git_dir()),
            real_oids,
            worktree_repos,
            errors: Vec::new(),
        })
    }

    #[test]
    fn status_result_updates_reported_worktree_instead_of_active_view() -> Result<()> {
        let root = unique_temp_dir("status-path");
        let main = root.join("main");
        let linked = root.join("linked");
        fs::create_dir_all(&main)?;

        let raw = git2::Repository::init(&main)?;
        commit_initial_file(&raw, Path::new("tracked.txt"), "base\n")?;
        raw.worktree("linked", &linked, None)?;

        let mut tab = RepoTab::open(&main)?;
        tab.refresh();
        assert!(!tab.commits.iter().any(|c| c.is_synthetic));

        tab.select_worktree(linked.clone());
        let linked_before = tab.worktree_views[&linked].status.clone();

        fs::write(main.join("tracked.txt"), "dirty\n")?;
        let dirty_status = GitRepo::open(&main)?.status()?;
        assert_eq!(dirty_status.total_files(), 1);

        tab.apply_status_result(StatusResult {
            main_path: Some(main.clone()),
            main_status: Some(dirty_status),
            staging_path: None,
            staging_status: None,
            staging_repo_state: git2::RepositoryState::Clean,
        });

        assert_eq!(tab.worktree_views[&main].status.total_files(), 1);
        assert_eq!(tab.worktree_views[&linked].status, linked_before);
        assert!(
            tab.commits
                .iter()
                .any(|c| { c.is_synthetic && c.synthetic_wt_name.as_deref() == Some("main") })
        );

        drop(tab);
        drop(raw);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[test]
    fn dirty_linked_worktree_survives_state_refresh_without_selecting_it() -> Result<()> {
        let root = unique_temp_dir("linked-dirty-state");
        let main = root.join("main");
        let linked = root.join("linked");
        fs::create_dir_all(&main)?;

        let raw = git2::Repository::init(&main)?;
        commit_initial_file(&raw, Path::new("tracked.txt"), "base\n")?;
        raw.worktree("linked", &linked, None)?;

        let repo = GitRepo::open(&main)?;
        let mut tab = RepoTab::open(&main)?;
        tab.apply_state_result(repo_state_result(&repo)?);
        assert!(tab.worktree_views[&linked].head_oid.is_some());
        tab.select_worktree(main.clone());
        assert_eq!(tab.active_worktree.as_deref(), Some(main.as_path()));

        fs::write(linked.join("tracked.txt"), "dirty\n")?;
        let dirty_status = GitRepo::open(&linked)?.status()?;
        assert_eq!(dirty_status.total_files(), 1);

        tab.apply_dirty_check_result(DirtyCheckResult::Worktree {
            tab_id: tab.id,
            path: linked.clone(),
            status: dirty_status,
        });
        assert!(
            tab.commits
                .iter()
                .any(|c| { c.is_synthetic && c.synthetic_wt_name.as_deref() == Some("linked") })
        );

        tab.apply_state_result(repo_state_result(&repo)?);
        assert!(
            tab.commits
                .iter()
                .any(|c| { c.is_synthetic && c.synthetic_wt_name.as_deref() == Some("linked") })
        );

        drop(tab);
        drop(raw);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }
}
