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

use anyhow::{Context, Result};

use std::sync::mpsc::Receiver;
use std::time::Instant;

use winit::event_loop::EventLoopProxy;

use crate::ci::{CiFetchResult, ProviderCiResult, ProviderCommitRollup};
use crate::commit_graph::GraphLayout;
use crate::config::Config;
use crate::git::{
    BranchTip, CommitInfo, CommitSubmoduleEntry, DiffFile, FullCommitInfo, GitRepo,
    RemoteOpResult, StashEntry, SubmoduleInfo, TagInfo, WorkingDirStatus, WorktreeInfo,
    insert_synthetics_sorted,
};
use crate::{github, gitlab, token_store};

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

    pub const ALL: [SidebarSection; 4] =
        [Self::Local, Self::Remote, Self::Tags, Self::Stashes];
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

    /// Re-query worktree-scoped state (status + branch + HEAD + submodules).
    pub fn refresh(&mut self) {
        self.status = self.repo.status().unwrap_or_default();
        self.current_branch = self.repo.current_branch().unwrap_or_default();
        self.head_oid = self.repo.head_oid().ok();
        self.submodules = self.repo.submodules().unwrap_or_default();
    }
}

pub struct RepoTab {
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

    // ---- Async ops ----
    pub fetch_op: Option<TimedOp>,
    pub pull_op: Option<TimedOp>,
    pub push_op: Option<TimedOp>,
    /// Working-tree mutation ops (cherry-pick, revert). Single slot
    /// shared across kinds since they all conflict with each other.
    pub mutation_op: Option<TimedOp>,

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

impl RepoTab {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let repo = GitRepo::open(&path).context("open repository")?;
        let mut tab = Self {
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
            fetch_op: None,
            pull_op: None,
            push_op: None,
            mutation_op: None,
            ci_results: Vec::new(),
            ci_receivers: Vec::new(),
            last_ci_fetch: None,
            last_push_time: None,
            ci_per_commit: HashMap::new(),
            diff_stats_rx: None,
            diff_stats_fetched: false,
            search_query: String::new(),
            nav_stack: Vec::new(),
            pinned_oid: None,
            pinned_path: None,
            repo,
        };
        tab.refresh();
        Ok(tab)
    }

    /// Re-query everything from the underlying repo. Synchronous; the
    /// async equivalent comes back when async polling is re-enabled.
    pub fn refresh(&mut self) {
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
        self.commits = self
            .repo
            .commit_graph_with_orphans(COMMIT_LIMIT)
            .or_else(|_| self.repo.commit_graph(COMMIT_LIMIT))
            .unwrap_or_default();

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
            if let Some(v) = entry {
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
            if let Some(v) = entry {
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
                .or_else(|| {
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                })
                .unwrap_or_default();
            let is_main = self
                .repo
                .workdir()
                .is_some_and(|wd| wd == path.as_path());
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
    /// event loop through `proxy`. Sets `last_ci_fetch` regardless of
    /// how many fetches actually launched, so the poll cadence backs off
    /// even when no remote is recognised.
    ///
    /// Tokens come from the system keychain via `token_store`. GitLab
    /// hosts seen here are also auto-registered into `config.gitlab_hosts`
    /// (and the config persisted) so the token modal has something to
    /// enumerate even before the user has set a token for the host.
    pub fn trigger_ci_fetch(&mut self, config: &mut Config, proxy: EventLoopProxy<()>) {
        self.last_ci_fetch = Some(Instant::now());
        let github_token = token_store::get_github_token();
        let mut seen_github = false;
        let mut seen_gitlab: HashSet<String> = HashSet::new();
        let mut config_dirty = false;
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
                self.ci_results
                    .retain(|r| r.provider != result.provider);
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
