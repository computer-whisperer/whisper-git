# Submodule & Worktree Deep Integration Design

**Version:** 1.0
**Date:** February 2026
**Status:** Proposal

---

## Problem Statement

Whisper-git currently treats submodules and worktrees as sidebar metadata — you can see them, right-click for basic operations, but actual work requires opening a separate tab (the GitKraken approach). This misses the core value proposition: these aren't separate repos, they're part of a unified project.

**Submodules** are nested repos with their own commit graphs, branches, and staging areas. Users need to manage them almost like full repos — but always in context of the parent.

**Worktrees** share the same commit graph and object database but have independent working directories, staging areas, and checked-out branches. Users switch between them for parallel development.

The goal: integrate both into the parent view so users never lose context.

---

## Design Principles

1. **Same graph, different lenses** — Worktrees are views into the same commit graph. Don't duplicate the graph; annotate it.
2. **Drill-down, not tab-away** — Submodules replace the main view temporarily with breadcrumb navigation back. Context is preserved, not abandoned.
3. **Parent context always visible** — When drilling into a submodule, keep a compressed parent view so users know where they are.
4. **Working directory is per-worktree** — The staging well must understand which worktree's changes it's showing.
5. **Progressive disclosure** — Basic info visible at a glance; details on interaction.

---

## Part 1: Worktree Integration

Worktrees share the same commit graph. The key insight: instead of showing worktrees as sidebar items only, annotate the graph itself to show where each worktree's HEAD is, and let the staging well switch between worktrees.

### 1.1 Worktree Pills in Graph

Add orange worktree pills alongside branch/tag pills on relevant commits:

```
    ●──── main  WT:main ─── Fix timeout  ── alice, 2h
    │
    ●──── feature/auth  WT:feature ─── Add OAuth  ── bob, 1d
    │
    ●──── v1.2  ─── Release prep  ── carol, 3d
```

**Visual treatment:**
- Color: Orange (#FF9800), matching the existing worktree indicator in sidebar
- Shape: Capsule pill, same as branch/tag pills
- Prefix: `WT:` to distinguish from branches (e.g., `WT:main`, `WT:feature`)
- Only shown for non-current worktrees (current worktree's HEAD is already shown by the HEAD pill)

**Data flow:**
- `WorktreeInfo` already has `branch` and `head_commit` fields
- During `layout_text()`, check if any worktree HEAD matches the current commit OID
- Render pill using existing `create_rounded_rect_vertices()` + outline

### 1.2 Staging Context Switching

The staging well currently shows the working directory of the repo that was opened. With worktrees, users need to stage/commit from any worktree.

**Worktree selector** — A dropdown at the top of the staging well:

```
+------ STAGING WELL ------------------------------------------+
|  Working on: [main ▾]                                        |
|  ┌─────────────────┐                                         |
|  │ ● main          │  <- current (green dot)                 |
|  │   feature       │                                         |
|  │   hotfix/crash  │                                         |
|  └─────────────────┘                                         |
|                                                              |
|  Commit Message                                              |
|  +-------------------------------------------------------+  |
|  | Fix authentication timeout                            |  |
|  +-------------------------------------------------------+  |
|  ...                                                         |
+--------------------------------------------------------------+
```

**Behavior:**
- Default: current worktree (the one whisper-git was opened from)
- Clicking the selector shows all worktrees with dirty indicator dots
- Switching worktree:
  - Saves current commit message draft (per-worktree)
  - Loads the selected worktree's staged/unstaged files
  - Restores that worktree's saved commit message draft
  - Updates the diff view if a file was selected
- Keyboard: `Ctrl+1`/`Ctrl+2`/etc. to switch worktrees by index

**Per-worktree state:**
```rust
struct WorktreeContext {
    path: PathBuf,
    branch: String,
    subject_draft: String,
    body_draft: String,
    staged_files: Vec<FileEntry>,
    unstaged_files: Vec<FileEntry>,
}
```

**Data flow:**
- `GitRepo` gains `open_worktree(path) -> Repository` to open a worktree-specific repo handle
- `status_files()` and `stage_file()`/`unstage_file()` accept an optional worktree path
- Commit operations target the selected worktree's index

### 1.3 Worktree Dirty Indicators in Graph

Extend the "Working (N)" row to show per-worktree status:

```
    +-- Working: main(3) feature(1) --+
    +--------+------------------------+
             |
    ●──── main  HEAD ─── Latest commit...
```

The working directory row shows all dirty worktrees with file counts. Clicking a specific worktree name in the row switches the staging well to that worktree.

### 1.4 Implementation Order

1. **Worktree pills in graph** — Annotate commits where worktree HEADs point. Visual-only, no interaction changes. ~200 lines.
2. **Staging context switching** — Worktree selector dropdown + per-worktree draft storage. ~400 lines.
3. **Working row enhancement** — Per-worktree dirty counts in the working directory row. ~100 lines.

---

## Part 2: Submodule Integration

Submodules are a harder problem. They have their own commit graphs, branches, remotes, and staging areas. The design uses a **drill-down / focus mode** that replaces the main view when navigating into a submodule, with breadcrumb navigation back and a compressed parent context strip.

### 2.1 Submodule Status Strip

Before drill-down, improve the at-a-glance submodule view. Add a compact status strip at the bottom of the graph area (or as a collapsible bar):

```
+-- SUBMODULES ──────────────────────────────────────────────+
│ embassy ●M +3  │  nanoarrow ●  │  trouble ●S  │  oggopus ● │
+────────────────────────────────────────────────────────────+
```

**Each pill shows:**
- Name (truncated if needed)
- Status dot: ● green (clean), ●M yellow (dirty), ●S blue (staged pointer change), ●D red (detached unexpected)
- Ahead/behind count relative to pinned commit (e.g., `+3` means 3 commits ahead of what parent expects)

**Interaction:**
- Click → drill into submodule (focus mode)
- Right-click → context menu (Update, Stage pointer, Open terminal)
- Hover → tooltip with branch, commit SHA, path

### 2.2 Focus Mode / Drill-Down

When a user clicks a submodule (from status strip, sidebar, or context menu), the main view transitions into that submodule's commit graph:

```
+-- BREADCRUMB ──────────────────────────────────────────────+
│  whisper-git  ›  embassy                              [✕]  │
+────────────────────────────────────────────────────────────+
+-- PARENT CONTEXT (40px) ──────────────────────────────────+
│  ●──●──●──●──●──●HEAD  main  (pinned: abc1234)           │
+──────────────────────────────────────────────────────────+
|                                                |          |
|     SUBMODULE GRAPH                            |  STAGING |
|     (embassy commit history)                   |  WELL    |
|                                                |  (embassy|
|     Full graph, branches, tags                 |  working |
|     Same rendering as parent                   |  dir)    |
|                                                |          |
+────────────────────────────────────────────────+──────────+
+-- SUBMODULE STATUS STRIP ─────────────────────────────────+
│ nanoarrow ●  │  trouble ●S  │  oggopus ●  │  (siblings)   │
+──────────────────────────────────────────────────────────+
```

**Components:**

#### Breadcrumb Bar
- Shows navigation path: `parent › submodule › nested-submodule`
- Each segment is clickable to navigate back
- `[✕]` button returns to parent (same as Escape)
- Keyboard: Escape pops one level, Ctrl+Escape returns to root

#### Parent Context Strip (40px compressed timeline)
- Compressed horizontal view of parent's recent commits
- Highlights the commit that pins this submodule (the commit in the parent repo that references the current submodule commit)
- Shows parent's HEAD and branch name
- Clickable to return to parent view
- Subtle, non-distracting — just enough to maintain spatial context

#### Submodule Graph
- Full commit graph rendering (reuses existing `CommitGraphView`)
- Shows the submodule's own branches, tags, and topology
- The **pinned commit** (what the parent expects) is highlighted with a special indicator:
  ```
      ●──── main ─── Latest work  ── alice, 2h
      │
      ●════ PINNED (by whisper-git) ═══ The commit parent expects  ── bob, 3d
      │
      ●──── Old work  ── carol, 1w
  ```
- If submodule HEAD is ahead of pinned: shows divergence count
- If submodule HEAD is behind pinned: shows warning

#### Staging Well
- Switches to show the submodule's working directory
- Commit operations work on the submodule's repo
- After committing in submodule, parent's status updates automatically (submodule pointer changed)

#### Sibling Submodule Strip
- Shows other submodules from the same parent at the bottom
- Click to switch directly between sibling submodules without returning to parent
- Preserves navigation depth (stay in drill-down mode)

### 2.3 Pinned Commit Coordination

When a user commits in a submodule while in focus mode, the parent needs to update its submodule pointer. The coordination workflow:

```
+-- SUBMODULE COMMIT COORDINATION ──────────────────────────+
│                                                            │
│  You committed in embassy (now at def5678)                 │
│  Parent whisper-git still pins abc1234                     │
│                                                            │
│  [Update parent pointer]  [Keep current pin]  [Return]     │
│                                                            │
+────────────────────────────────────────────────────────────+
```

**"Update parent pointer":**
1. Stages the submodule pointer change in the parent's index
2. Returns to parent view
3. Parent's staging well shows the submodule pointer update as a staged change

This closes the loop: submodule commit → parent pointer update → parent commit — all without leaving the app.

### 2.4 Nested Submodule Support

Submodules can contain submodules. The breadcrumb handles arbitrary depth:

```
whisper-git  ›  embassy  ›  embassy-net
```

Each level pushes onto a navigation stack. The parent context strip always shows the immediate parent. Implementation uses a stack:

```rust
struct NavigationStack {
    entries: Vec<NavigationEntry>,
}

struct NavigationEntry {
    repo_path: PathBuf,
    repo_name: String,
    // Saved view state for restoration
    scroll_offset: f32,
    selected_commit: Option<Oid>,
}
```

### 2.5 Implementation Order

1. **Submodule status strip** — Compact pills at bottom of graph area. Visual-only initially. ~300 lines.
2. **Drill-down navigation** — Open submodule graph in place, with navigation stack. ~500 lines.
3. **Breadcrumb bar** — Navigation UI + Escape to pop. ~200 lines.
4. **Parent context strip** — Compressed parent timeline. ~300 lines.
5. **Pinned commit highlighting** — Special indicator on the pinned commit in submodule graph. ~100 lines.
6. **Commit coordination** — Post-commit dialog for updating parent pointer. ~200 lines.

---

## Part 3: Architecture

### 3.1 Navigation Model

```rust
enum ViewMode {
    /// Normal view — parent repo graph + staging
    Root,
    /// Drilled into a submodule
    SubmoduleFocus {
        /// Stack of parent repos (for nested submodules)
        parent_stack: Vec<NavigationEntry>,
        /// Current submodule repo
        submodule_repo: GitRepo,
        /// Pinned commit OID (what parent expects)
        pinned_oid: Oid,
        /// Sibling submodule info (for status strip)
        siblings: Vec<SubmoduleInfo>,
    },
}
```

`ViewMode` lives on `TabViewState`. When in `SubmoduleFocus`, the rendering pipeline uses `submodule_repo` for graph/staging instead of `RepoTab.repo`.

### 3.2 Rendering Changes

The three-layer rendering model (graph → chrome → overlay) stays the same. New components slot in:

- **Breadcrumb bar** → chrome layer, replaces tab bar space when in focus mode
- **Parent context strip** → chrome layer, between breadcrumb and graph
- **Submodule status strip** → chrome layer, below graph area
- **Worktree pills** → graph layer, alongside existing branch/tag pills
- **Worktree selector** → chrome layer, top of staging well

### 3.3 Data Flow

```
User clicks submodule
  → Push current state onto NavigationStack
  → Open submodule repo via GitRepo::open(submodule_path)
  → Load submodule commits via commit_graph(N)
  → Find pinned commit OID from parent's index
  → Set ViewMode::SubmoduleFocus
  → Render submodule graph + staging

User presses Escape
  → Pop NavigationStack
  → Restore parent state (scroll, selection)
  → Set ViewMode::Root (or parent SubmoduleFocus for nested)
  → Re-render parent view
```

### 3.4 GitRepo Changes

```rust
impl GitRepo {
    /// Open a submodule as a full repo for graph/staging operations
    pub fn open_submodule_repo(&self, name: &str) -> Result<GitRepo>;

    /// Get the OID that the parent pins this submodule to
    pub fn submodule_pinned_oid(&self, name: &str) -> Result<Oid>;

    /// Stage a submodule pointer update (after submodule commit)
    pub fn stage_submodule_update(&self, name: &str) -> Result<()>;

    /// Open a worktree as a repo handle for staging operations
    pub fn open_worktree_repo(&self, path: &Path) -> Result<Repository>;

    /// Get status files for a specific worktree
    pub fn worktree_status(&self, path: &Path) -> Result<Vec<FileEntry>>;
}
```

---

## Part 4: Visual Summary

### Before (Current)

```
+───────────────────────────────────────────────────────────+
│ Header: repo | branch | Fetch Pull Push Commit        ? = │
+───────────────────────────────────────────────────────────+
│ SIDEBAR    │              │                               │
│            │  GRAPH       │  STAGING                      │
│ LOCAL      │              │                               │
│ REMOTE     │  (parent     │  (parent working dir)         │
│ TAGS       │   only)      │                               │
│ SUBMODULES │              ├───────────────────────────────│
│ WORKTREES  │              │  DIFF / DETAIL                │
│ STASHES    │              │                               │
+───────────────────────────────────────────────────────────+
```

Submodules and worktrees are sidebar metadata only.

### After (Proposed) — Root View

```
+───────────────────────────────────────────────────────────+
│ Header: repo | branch | Fetch Pull Push Commit        ? = │
+───────────────────────────────────────────────────────────+
│ SIDEBAR    │              │  Working on: [main ▾]         │
│            │  GRAPH       │  STAGING (per-worktree)       │
│ LOCAL      │  (with WT:   │                               │
│ REMOTE     │   pills)     │  (selected worktree's         │
│ TAGS       │              │   staged/unstaged files)      │
│ SUBMODULES │              ├───────────────────────────────│
│ WORKTREES  │              │  DIFF / DETAIL                │
│ STASHES    │              │                               │
├────────────┴──────────────┴───────────────────────────────│
│ SUBMODULE STATUS STRIP: embassy ●M +3 │ nanoarrow ● │ ...│
+───────────────────────────────────────────────────────────+
```

### After (Proposed) — Submodule Focus Mode

```
+───────────────────────────────────────────────────────────+
│ BREADCRUMB: whisper-git › embassy                    [✕]  │
+───────────────────────────────────────────────────────────+
│ PARENT: ●──●──●──●──●HEAD main  (pinned: abc1234)        │
+───────────────────────────────────────────────────────────+
│ SIDEBAR    │              │  Working on: [default]        │
│            │  SUBMODULE   │  STAGING (submodule)          │
│ LOCAL      │  GRAPH       │                               │
│ REMOTE     │  (embassy    │  (submodule working dir)      │
│ TAGS       │   commits)   │                               │
│            │              ├───────────────────────────────│
│            │  ══PINNED══  │  DIFF / DETAIL                │
│            │              │                               │
├────────────┴──────────────┴───────────────────────────────│
│ SIBLINGS: nanoarrow ● │ trouble ●S │ oggopus ●            │
+───────────────────────────────────────────────────────────+
```

---

## Part 5: Implementation Phases

### Phase 1: Worktree Graph Annotations (~200 lines, 1 sprint)
- Add `WT:name` pills to commits where worktree HEADs point
- Orange color, matching existing worktree theme
- Data already available in `WorktreeInfo.head_commit`

### Phase 2: Submodule Status Strip (~300 lines, 1 sprint)
- Compact pill bar below graph area
- Shows name + status dot + ahead/behind count
- Click opens context menu (Update, Stage pointer, Open terminal)
- Layout: fixed 32px height strip, scrollable horizontally if many submodules

### Phase 3: Staging Context Switching (~400 lines, 1 sprint)
- Worktree selector dropdown in staging well
- Per-worktree commit message draft storage
- `GitRepo::worktree_status()` for loading worktree-specific files
- Keyboard shortcuts for switching (Ctrl+1/2/3)

### Phase 4: Submodule Drill-Down (~700 lines, 1-2 sprints)
- `NavigationStack` + `ViewMode` enum
- Open submodule as full `GitRepo`
- Breadcrumb bar with click-to-navigate
- Escape to pop navigation
- Sidebar switches to show submodule's branches

### Phase 5: Parent Context & Coordination (~500 lines, 1 sprint)
- Compressed parent timeline strip (40px)
- Pinned commit highlighting in submodule graph
- Post-commit coordination dialog
- `stage_submodule_update()` for one-click parent pointer update

### Total estimate: ~2,100 lines across 4-5 sprints

---

## Open Questions

1. **Worktree selector position** — Top of staging well vs. header bar? Staging well keeps it near the files; header bar is more discoverable.
2. **Parent context strip detail level** — Should it show just a timeline, or mini commit messages too?
3. **Nested submodule depth limit** — Should we cap navigation depth? (Practical: 2-3 levels)
4. **Worktree creation from graph** — "Create worktree here" on commit context menu? (Natural companion to "Create branch here")

---

## References

- User needs: `docs/user_needs.md`
- Design feedback: `docs/design_feedback_feb2026.md` (worktree-centric paradigm, constellation view)
- UX design: `docs/ux-design-2026-02.md` (secondary repos panel, screen layout)
- Current sidebar: `src/views/branch_sidebar.rs` (SUBMODULES/WORKTREES sections)
- Git operations: `src/git.rs` (SubmoduleInfo, WorktreeInfo, is_dirty)
