# Whisper-Git UX Design

**Version:** 1.0
**Date:** February 2026

---

## Overview

Whisper-Git is a Vulkan-rendered git client optimized for developers managing repositories with submodules and multiple worktrees. The interface prioritizes:

- **At-a-glance status** across all repositories
- **Information density** without visual clutter
- **Keyboard and mouse parity** — both input methods fully capable
- **Stable, predictable layout** — panels don't shift based on state

This is a rich GUI application with smooth spline-rendered commit graphs, not a TUI.

---

## Design Principles

1. **Primary repo with commit graph always visible** — the graph is the main interface, not a sidebar decoration
2. **Secondary repos scannable at a glance** — submodules and worktrees shown as compact cards, focusable when needed
3. **Working directory state prominent** — dirty repos surface automatically
4. **Current staging work always visible** — no mode switching to see what you're about to commit
5. **One place for each piece of information** — no redundant displays
6. **On-demand over always-visible** — details appear when relevant
7. **Standard patterns** — context menus, command palettes, familiar interactions

---

## 1. Screen Layout

```
+-----------------------------------------------------------------------------------+
|  HEADER: Repository name | Branch | Sync status | [Fetch] [Push] [Commit]    [?][=]|
+-----------------------------------------------------------------------------------+
|                                                |                                  |
|                                                |     STAGING WELL                 |
|     PRIMARY GRAPH                              |                                  |
|                                                |     Commit message + staged      |
|     Commit topology with branches,             |     files + unstaged files       |
|     markers, working directory node            |                                  |
|                                                +----------------------------------+
|                                                |                                  |
|                                                |     SECONDARY REPOS              |
|                                                |                                  |
|                                                |     Cards for submodules and     |
|                                                |     worktrees, problems first    |
|                                                |                                  |
+-----------------------------------------------------------------------------------+
```

### Size Ratios (16:9 display)

| Region | Percentage | Purpose |
|--------|-----------|---------|
| Header | 4% height | Context, sync status, primary actions |
| Primary Graph | 55% width, 96% height | Main interaction surface |
| Staging Well | 45% width, 45% height | Current commit work |
| Secondary Repos | 45% width, 51% height | Repository cards |

The primary graph dominates the left side. The right side stacks the staging well (what you're working on now) above the secondary repos panel (other repositories you're tracking).

---

## 2. The Primary Graph

The commit graph is rendered using GPU-accelerated spline curves for smooth branch visualization at any zoom level.

### Information Layers

**Always visible:**
- Branch topology as spline curves
- Branch tips with solid color labels
- Remote positions with outlined labels (e.g., `origin/main`)
- Tags as diamond markers
- HEAD with prominent indicator and current branch name
- Time scale on left edge

**On selection/hover:**
- Full commit message
- File change summary
- Full SHA
- Author and timestamp

### Commit Display

Each commit shows essential information inline:

```
* abc1234 Fix auth timeout +45/-12 -- alice, 2h
```

Components:
- **Node** (8px circle, colored by branch)
- **Short SHA** (7 characters)
- **Message** (first line, truncated if needed)
- **Change size** (`+N/-M` in muted color)
- **Author** and **relative time**

Merge commits use a double-ring indicator to distinguish them visually.

### Branch Colors

| Color | Hex | Usage |
|-------|-----|-------|
| Blue | #3B82F6 | Primary branch (main/master) |
| Green | #22C55E | Feature branches |
| Amber | #F59E0B | Release branches |
| Purple | #A855F7 | Hotfix branches |
| Slate | #64748B | Remote tracking refs |

Branch lines use 3px stroke for the main branch, 2px for others.

### The Working Directory Node

When the working directory has uncommitted changes, a special node appears above HEAD:

```
    +---------------+
    | Working (5)   |    <- Dashed border when dirty
    +-------+-------+
            |
            | (dashed line)
            |
    +-------+-------+
    | abc1234       |    <- HEAD commit
    | Fix timeout   |
    +---------------+
```

The number indicates modified file count. Clicking this node focuses the staging well.

### Header Bar

```
whisper-git | main | [Fetch] [Push (+3)] [Commit]                    [?][=]
```

- **Repository name** and **current branch**
- **Action buttons** with contextual badges:
  - `Push (+3)` when commits are ahead of remote
  - `Pull (-2)` when commits are behind
  - Buttons show spinners during operations
- **Help** (`?`) and **settings** (`=`) access

---

## 3. The Staging Well

The staging well shows everything related to your current commit-in-progress.

```
+------ STAGING WELL ------------------------------------------+
|                                                              |
|  Commit Message                                              |
|  +-------------------------------------------------------+  |
|  | Fix authentication timeout handling                    |  |
|  +-------------------------------------------------------+  |
|  | The token refresh had a race condition...              |  |
|  +-------------------------------------------------------+  |
|  47 chars                                                    |
|                                                              |
|  Suggested: "Auth timeout fix + retry logic"  [Use]         |
|                                                              |
|  Staged (2 files)                                   +45 -12  |
|  +-------------------------------------------------------+  |
|  | src/auth/login.rs                            +42  -10  |  |
|  | src/auth/mod.rs                              +3   -2   |  |
|  +-------------------------------------------------------+  |
|                                                              |
|  Unstaged (3 files)                                  +8  -0  |
|  +-------------------------------------------------------+  |
|  | src/main.rs                                  +8   -0   |  |
|  | src/config.rs                                +0   -0   |  |
|  | src/utils.rs                                 +0   -0   |  |
|  +-------------------------------------------------------+  |
|                                                              |
|  [Stage All]  [Unstage All]                        [Commit]  |
|                                                              |
+--------------------------------------------------------------+
```

### Components

- **Commit message editor** — subject line and optional body, with character count
- **LLM-suggested message** — auto-generated summary of staged changes (see Section 8)
- **Staged files** — files ready to commit, with `+/-` line counts
- **Unstaged files** — modified files not yet staged
- **Action buttons** — Stage All, Unstage All, Commit

Clicking a file shows its diff. Files can be dragged between staged and unstaged.

---

## 4. Secondary Repos Panel

This panel tracks submodules, worktrees, and other related repositories.

```
+------ SECONDARY REPOS ------------------------------------+
|                                                           |
|  --- 1 repo needs attention ---                           |
|  +--- lib-ui --------------------+                        |
|  |  [!!] dirty    feature/btn    |                        |
|  |      * def456                 |                        |
|  |      |                        |                        |
|  +-------------------------------+                        |
|  ----------------------------------------                 |
|  +--- lib-crypto -----+  +--- lib-network -+              |
|  |  [OK]    main      |  |  [OK]    main   |              |
|  |      * abc123      |  |      * ghi789   |              |
|  +--------------------+  +-----------------+              |
|                                                           |
+-----------------------------------------------------------+
```

### Card Contents

Each repository card shows:
- **Name** and **current branch**
- **Status badge**: `[OK]`, `[+N]` (ahead), `[-N]` (behind), `[!!]` (dirty/conflict)
- **Miniature graph** showing HEAD and 2-3 recent commits

### Problem Repos Surface to Top

Repositories needing attention automatically sort to the top with a separator line. Sort order:

1. Dirty or conflicts (red `[!!]`)
2. Behind remote (amber `[-N]`)
3. Ahead of remote (blue `[+N]`)
4. Clean (green `[OK]`)

This ensures you can't miss a problem regardless of how many repos you're tracking.

### Card Interaction

- **Click** — Expand card to show file list and actions
- **Double-click** — Focus this repo in the primary graph (replaces main view)
- **Escape** — Return to previous view

---

## 5. Actions and Commands

### Command Palette

**Invocation:** `Ctrl+P` or `:`

```
+------ COMMAND PALETTE -----------------------------------+
|                                                          |
|  > commit_                                               |
|                                                          |
|  Recent                                                  |
|  > Commit                                    Ctrl+Enter  |
|  > Push                                              p   |
|  > Fetch                                             f   |
|                                                          |
|  Matching                                                |
|  > Commit: Amend                                         |
|  > Commit: Sign                                          |
|                                                          |
+----------------------------------------------------------+
```

Features:
- **Fuzzy matching** — "cm" matches "Commit"
- **Recent commands** shown before typing
- **Keyboard shortcuts** displayed inline
- **Categories**: Git, View, Navigation, Settings

The command palette is the universal escape hatch — every action is accessible through it.

### Context Menus

Right-click shows context-appropriate actions:

**On commit:**
- Checkout
- Cherry-pick
- Revert
- Create branch here
- Create tag here
- Copy SHA

**On branch:**
- Checkout
- Merge into current
- Rebase onto current
- Delete
- Rename

**On staged file:**
- Unstage
- View diff
- Discard changes

### Header Buttons

| Button | Badge |
|--------|-------|
| Fetch | Spinner during operation |
| Push | `(+N)` when commits ahead |
| Commit | Highlighted when files staged |

---

## 6. Keyboard Navigation

Vim-style keys are the primary navigation method, but arrow keys work everywhere as alternatives. Mouse is fully functional for all operations.

### Global

| Key | Action |
|-----|--------|
| `Tab` | Cycle focus: Graph → Staging → Secondary |
| `/` | Search commits |
| `:` or `Ctrl+P` | Command palette |
| `?` | Show keyboard shortcuts |
| `Ctrl+Enter` | Commit |
| `Ctrl+Shift+Enter` | Commit and push |
| `Escape` | Close/back |

### Graph

| Key | Action |
|-----|--------|
| `j` / `k` | Next/previous commit |
| `J` / `K` | Jump 10 commits |
| `Enter` | Show commit detail |
| `[` / `]` | Previous/next branch tip |
| `g` | Go to HEAD |

### Staging

| Key | Action |
|-----|--------|
| `j` / `k` | Next/previous file |
| `Space` | Toggle staged |
| `a` | Stage all |
| `Enter` | Show diff |

---

## 7. Color System

### Status Colors

| Status | Color | Hex | Badge |
|--------|-------|-----|-------|
| Clean | Green | #22C55E | `[OK]` |
| Behind | Amber | #F59E0B | `[-N]` |
| Dirty | Red | #EF4444 | `[!!]` |
| Ahead | Blue | #3B82F6 | `[+N]` |

### Attention Levels

| Level | When | Visual Treatment |
|-------|------|------------------|
| Normal | Standard state | Standard colors |
| Attention | Needs action | Badge + surfaces to top |

### Themes

**Dark (default):**
```
Background:    #0F172A
Surface:       #1E293B
Border:        #334155
Text:          #F8FAFC
Text muted:    #94A3B8
```

**Light:**
```
Background:    #F8FAFC
Surface:       #FFFFFF
Border:        #E2E8F0
Text:          #0F172A
Text muted:    #64748B
```

---

## 8. LLM-Generated Commit Suggestions

A lightweight LLM (Claude Haiku or a local model) generates human-readable summaries of staged changes to suggest commit messages.

### How It Works

1. When files are staged, the diff is sent to the LLM in the background
2. The model generates a concise summary (max 60 characters)
3. The suggestion appears in the staging well: `Suggested: "Auth timeout fix + retry logic" [Use]`
4. Click `[Use]` to insert as commit message, or ignore it

### Technical Details

- **Runs asynchronously** — never blocks the UI
- **Cached by content hash** — regenerates only when staged content changes
- **User-controllable** — can be disabled in settings
- **Model selection** — choose between cloud API or local model for offline use

### Input Context

The LLM receives:
- File names changed
- Diff hunks (truncated if large)
- Recent commit messages (for style matching)

### Output Constraints

- Imperative mood ("Fix bug" not "Fixed bug")
- Max 60 characters
- No period at end

---

## 9. Mockups

### Clean State

```
+-----------------------------------------------------------------------------------+
|  whisper-git | main | [Fetch] [Push] [Commit]                                [?][=]|
+-----------------------------------------------------------------------------------+
|                                                |                                  |
|     * abc1234 Fix auth +45/-12 -- you, 2h HEAD |  Commit Message                  |
|     |                            <- origin/main|  +------------------------------+|
|     * def5678 Add retry +120/-30 -- alice, 1d  |  | No staged changes            ||
|     |                                          |  +------------------------------+|
|     * ghi9012 Refactor +380/-220 -- bob, 3d    |                                  |
|     |                                 <- v1.2  |  Unstaged: 0 files               |
|     +---* feature/auth (3 ahead)               |                                  |
|     |   |                                      +----------------------------------+
|     |   * Auth work +25/-5                     |  +-- lib-crypto --+ +-- lib-ui -+|
|     |                                          |  |  [OK]   main   | |  [OK] main||
|     * jkl0123 Ancient +2/-1 -- carol, 1w       |  |     * abc123   | |    * def45||
|     |                                          |  +----------------+ +------------+|
|                                                |                                  |
+-----------------------------------------------------------------------------------+
```

### Working State (Uncommitted Changes)

```
+-----------------------------------------------------------------------------------+
|  whisper-git | main | [Fetch] [Push (+3)] [Commit]                           [?][=]|
+-----------------------------------------------------------------------------------+
|                                                |                                  |
|     +-- Working (5) --+                        |  Commit Message                  |
|     +--------+--------+             <- dirty   |  +------------------------------+|
|              |                                 |  | Fix authentication timeout   ||
|     * abc1234 Fix auth +45/-12 -- you, 2h HEAD |  +------------------------------+|
|     |                            <- origin/main|  | The token refresh had...     ||
|     * def5678 Add retry +120/-30 -- alice, 1d  |  +------------------------------+|
|     |                                          |  47 chars                        |
|                                                |                                  |
|                                                |  Suggested: "Auth timeout fix"  |
|                                                |                                  |
|                                                |  Staged (2)              +45 -12|
|                                                |  +------------------------------+|
|                                                |  | login.rs             +42 -10 ||
|                                                |  | mod.rs               +3  -2  ||
|                                                |  +------------------------------+|
|                                                |  Unstaged (3)            +8  -0 |
|                                                |  +------------------------------+|
|                                                |  | main.rs              +8  -0  ||
|                                                |  +------------------------------+|
|                                                |                                  |
|                                                |  [Stage All] [Unstage] [Commit] |
+-----------------------------------------------+----------------------------------+
|                                                |  --- 1 needs attention ---       |
|                                                |  +-- lib-ui ----------------+    |
|                                                |  |  [!!] dirty  feature/btn |    |
|                                                |  +---------------------------+   |
|                                                |  -------------------------       |
|                                                |  +-- lib-crypto -+ +-- lib-net -+|
|                                                |  |  [OK]  main   | |  [OK] main ||
|                                                |  +---------------+ +------------+|
+-----------------------------------------------------------------------------------+
```

---

## 10. Performance

### Targets

| Metric | Target |
|--------|--------|
| Input response | <16ms (same frame) |
| Graph render (1000 commits) | <50ms |
| Selection update | <10ms |
| Full layout reflow | <100ms |

### Rendering Strategy

- **Virtualized commit list** — only visible commits (plus buffer) are rendered
- **GPU-accelerated splines** — branch curves rendered via Vulkan
- **Progressive loading** — structure first, then details
- **Async LLM** — suggestions generated in background, never blocking

---

## 11. Future Work

These features require separate design efforts:

1. **Merge conflict resolution** — three-way diff visualization
2. **Interactive rebase** — drag-and-drop commit reordering
3. **Blame view** — per-line history
4. **Settings panel** — preferences and configuration
5. **Pull request integration** — GitHub/GitLab workflows

---

## Summary

| Goal | Implementation |
|------|----------------|
| See commit graph | 55% of screen, spline-rendered topology |
| See secondary repos | Cards with status badges, problems surface to top |
| See dirty state | Red `[!!]` badge, automatic surfacing |
| See staging work | Dedicated panel with LLM suggestions |
| Navigate by keyboard | Vim-style keys, command palette |
| Navigate by mouse | Click, right-click menus, drag |
