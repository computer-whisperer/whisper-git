# Whisper-Git UX Design Document

## Vision & Philosophy

### What Makes Whisper-Git Different

Existing git visualization tools fall into two camps: terminal tools that show topology but lack interactivity, and GUI tools that bury topology under lists and panels. Whisper-git takes a different approach: **the commit graph is the interface**.

Most git GUIs treat the commit graph as a decoration next to a commit list. We flip that relationship. The graph is primary—a navigable, zoomable landscape where commits are places you can visit, not rows you scroll past.

**This is not just a viewer—it's an editor.** You can stage files, write commits, and push to remotes without leaving the graph interface. The working directory state is always visible, not hidden in a separate panel.

### Core Metaphor: The Frayed Rope

Most repositories aren't sprawling networks—they're **frayed ropes**. A main strand runs through history, with occasional parallel strands that split off and rejoin. The interesting questions aren't "what's the shape?" but:

- **Where are the markers?** Branch tips, tags, remote tracking positions
- **What's running in parallel?** Feature branches, release branches, hotfixes
- **What's the state of dependencies?** Submodule versions, their alignment

Think of your repository as a rope you're examining:
- **The main strand** is your primary branch (main/master/develop)
- **Frayed sections** are where parallel work happens
- **Markers tied to the rope** are branches, tags, and remote positions
- **Attached smaller ropes** are submodules, each with their own state

This isn't decoration. It's how the tool thinks. Every interaction reinforces understanding of where things are positioned along your history.

### Design for Understanding

The goal isn't to make git operations faster (though that's a side effect). The goal is to make git **understandable**. When you can see that feature-X branched from main three weeks ago and has diverged by 47 commits, you understand your situation in a way that `git log --oneline` never conveys.

---

## Core Views

### A. Overview (Zoomed Out)

**Purpose:** See the full rope—where all markers are positioned along your history.

At maximum zoom out, individual commits collapse but **markers remain visible**:

**Always Visible at Overview:**
- **Branch positions**: Where each local branch points (with name labels)
- **Remote tracking**: Where origin/main, origin/feature-x, etc. sit relative to local
- **Tags**: Release tags, version markers along the timeline
- **Divergence indicators**: Visual gaps showing where local and remote differ
- **Submodule status strip**: Compact row showing each submodule's state

**The Rope Structure:**
- Main strand rendered as the thickest line
- Parallel strands (active branches) shown as thinner lines running alongside
- Merged strands rejoin visibly
- Commit density shown via line thickness or shading (many commits = bolder)

**Markers Along the Rope:**
```
         origin/main    HEAD (main)     feature/auth
              ↓              ↓               ↓
═══════●══════●══════════════●───────────────●
                              ╲             ╱
                               ●───●───●───●
                                 hotfix/login
```

**Submodule Status Strip:**
At overview zoom, a thin strip at the bottom shows submodule health:
- `lib-crypto: ✓ abc123` — clean, matches committed reference
- `lib-network: ↑ def456` — local changes, ahead of committed
- `lib-ui: ⚠ ghi789` — mismatch, needs attention

**What This View Is For:**
- "Where is origin/main vs my local main?"
- "Which branches have unpushed work?"
- "Are my submodules in sync?"
- Quick-jump to any marker by clicking it

### B. Topology View (Primary Working View)

**Purpose:** Navigate the commit graph with full detail—see exactly how strands weave.

This is the primary working view. At this zoom level you see individual commits:

**Layout Philosophy:**
- **Main strand stays central**: The primary branch runs down the middle
- **Parallel strands offset**: Feature branches run alongside, not scattered
- **Spline connections**: Smooth curves show parent-child, merges visible as joins
- **Compact vertical spacing**: Maximize commits visible without scrolling

**What's Visible Per Commit:**
```
●─ abc1234 Fix auth timeout — alice, 3d ago
│
●─ def5678 Add retry logic — alice, 3d ago  ← origin/main
│
●─ ghi9012 Refactor client — bob, 5d ago    ← v1.2.0
```

**Branch/Tag Markers:**
- Rendered inline with commits they point to
- Local branches: solid label
- Remote tracking: outlined label with remote icon
- Tags: distinct shape (flag/diamond)
- Multiple markers on same commit stack vertically

**Visual Distinction:**
- Current HEAD: Filled circle with ring
- Branch tips: Labels always visible
- Merge commits: Larger node, shows both parents
- Regular commits: Standard node

**Parallel Strand Rendering:**
When branches run in parallel, they're shown as adjacent lanes:
```
main                    feature/auth
  │                          │
  ●─ Merge feature/auth ─────┤
  │                          ●─ Final cleanup
  │                          │
  │                          ●─ Add tests
  ●─ Unrelated fix           │
  │                          ●─ Implement auth
  ├──────────────────────────●─ Branch point
  │
```

### C. Timeline View

**Purpose:** Understand repository activity over time.

Rotates the mental model from topology to chronology:

- **Horizontal timeline**: Time flows left to right
- **Vertical stacking**: Commits from same time period stack vertically
- **Grouping**: Automatic clustering by day/week/month based on zoom
- **Activity heatmap**: Background intensity shows commit frequency

**Filters:**
- By branch (show only main, show all, show selected)
- By author (single author mode for "what did I do?")
- By path (commits touching specific files/directories)

**Use Cases:**
- "When was this feature developed?"
- "Who was working on this last month?"
- "What was the commit cadence before the release?"

### D. Contributor View

**Purpose:** Understand who works on what.

Groups commits by author rather than branch:

- **Author lanes**: Each contributor gets a horizontal lane
- **Activity patterns**: Visual rhythm of contributions over time
- **Contribution areas**: Heatmap of which files/modules each person touches
- **Collaboration points**: Where different authors' commits interleave

**Use Cases:**
- Code review routing ("who knows this area?")
- Team dynamics understanding
- Bus factor visualization

### E. Submodule Integration (Not a Separate View)

**Philosophy:** Submodules aren't hidden—they're visible context alongside your main graph.

Rather than a separate "submodule view," submodule state is **always present** via the submodule strip.

**Submodule Strip (Always Visible):**
A persistent horizontal strip showing all submodules:

```
┌─────────────────────────────────────────────────────────────┐
│ lib-crypto ✓ abc123  │  lib-network ↑2 def456  │  lib-ui ⚠ │
└─────────────────────────────────────────────────────────────┘
```

**Status Indicators:**
- `✓` Clean: Submodule matches committed reference
- `↑N` Ahead: Submodule has N local commits beyond reference
- `↓N` Behind: Submodule is N commits behind available updates
- `⚠` Mismatch: Submodule HEAD differs from what parent expects
- `✗` Missing: Submodule not initialized or path missing

**Interaction:**
- **Hover submodule**: Tooltip shows full SHA, last commit message, update time
- **Click submodule**: Expand inline to show recent commits
- **Double-click**: Enter submodule context (full graph for that submodule)
- **Right-click**: Update, init, sync, open in terminal

**Expanded Inline View:**
Clicking a submodule expands it in place:
```
┌─ lib-crypto ─────────────────────────────────────┐
│  ● abc123 Fix buffer overflow — eve, 2d ago      │
│  │                                               │
│  ● fed098 Add encryption tests — eve, 3d ago     │
│  │                                    ↑ parent   │
│  ● cba765 Initial crypto impl — eve, 1w ago      │
└──────────────────────────────────────────────────┘
```

The `↑ parent` marker shows which commit the parent repo has pinned.

**Submodule in Commit Context:**
When viewing a commit that changed submodule references:
- Show old→new SHA transition
- One-click to see what commits that represents
- Warning if the update skipped commits (non-fast-forward)

---

## Working Directory & Editing

Whisper-git is an editor, not just a viewer. **The working directory is the primary view when you have uncommitted changes.**

### Layout Philosophy: Work First, History Second

When your working directory is clean, the commit graph dominates. But when you have changes, the layout shifts:

```
┌─────────────────────────────────────────────────────────────────────┐
│                                                                     │
│                     Working Directory (60%)                         │
│                                                                     │
│   Staged / Modified / Untracked files                              │
│   Diff view for selected file                                       │
│                                                                     │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│                     Commit Graph (40%)                              │
│                                                                     │
│   Recent commits, current position                                  │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

**The Principle:** What you're doing now is more important than what you did before. History provides context, but your current work is the focus.

**Automatic Layout Switching:**
- **Clean working directory**: Graph takes full space, minimal status indicator
- **Changes present**: Split view, working directory on top (or left, configurable)
- **Commit dialog open**: Commit interface takes focus, graph fades to context

**Manual Override:**
- `Tab`: Toggle between working directory focus and graph focus
- `1`: Force graph-only view (history mode)
- `2`: Force split view (working mode)

### Working Directory Panel (Primary)

When changes exist, this is the main interface—a two-pane view with file list and live diff:

```
┌─ Working Directory ──────────────────────────────────────────────────────────┐
│                                                                              │
│  Staged (2)                               │ ┌─ src/auth/login.rs ──────────┐│
│    ✓ src/auth/login.rs    +45 -12  ←      │ │ @@ -15,6 +15,14 @@           ││
│    ✓ src/auth/mod.rs      +3  -0          │ │   fn authenticate() {        ││
│                                           │ │ +     validate_token()?;     ││
│  Modified (3)                             │ │ +     refresh_session();     ││
│    ● src/main.rs          +8  -2          │ │       load_user_data()       ││
│    ● src/config.rs        +15 -5          │ │   }                          ││
│    ● tests/auth_test.rs   +30 -0          │ │                              ││
│                                           │ │ @@ -45,3 +53,12 @@           ││
│  Untracked (2)                            │ │   fn logout() {              ││
│    ? src/auth/oauth.rs                    │ │ +     clear_session();       ││
│    ? notes.txt                            │ │ +     invalidate_token();    ││
│                                           │ │   }                          ││
│                                           │ └────────────────────────────────┘│
│  ─────────────────────────────────────────┴──────────────────────────────────│
│  [a] Stage All   [c] Commit   [z] Stash   [d] Discard                        │
└──────────────────────────────────────────────────────────────────────────────┘
```

**Two-Pane Design:**
- Left (40%): File list grouped by status
- Right (60%): Diff for selected file, updates as you navigate
- Bottom: Action bar with keyboard hints

**File Status Indicators:**
- `✓` Staged (green) — ready to commit
- `●` Modified, unstaged (yellow) — changes not yet staged
- `?` Untracked (gray) — new file, not tracked
- `!` Conflicted (red) — merge conflict, needs resolution
- `D` Deleted — file removed
- `R` Renamed — file moved/renamed (shows old → new)

**Navigation:**
| Key | Action |
|-----|--------|
| `j`/`k` | Move through files |
| `Space` or `s` | Toggle staged/unstaged for selected file |
| `a` | Stage all files |
| `u` | Unstage all files |
| `d` | Discard changes (with confirmation) |
| `c` | Open commit dialog |
| `z` | Stash changes |
| `Tab` | Switch focus between file list and diff pane |
| `Esc` | Back to graph (if clean) or minimize working directory |

**Diff Pane Features:**
- Syntax highlighting for known file types
- Hunk-level staging: `s` on a hunk stages just that hunk
- Line-level staging: Select lines, then `s` to stage selection
- `n`/`p`: Jump to next/previous hunk
- `e`: Open file in external editor at current line

### Commit Interface

Press `c` (with staged changes) to replace the diff pane with the commit interface:

```
┌─ Working Directory ──────────────────────────────────────────────────────────┐
│                                                                              │
│  Staged (2)                               │ ┌─ New Commit ─────────────────┐│
│    ✓ src/auth/login.rs    +45 -12         │ │                              ││
│    ✓ src/auth/mod.rs      +3  -0          │ │ Subject:                     ││
│                                           │ │ Fix authentication timeout_  ││
│  Modified (1)                             │ │                              ││
│    ● src/config.rs        +2  -0          │ │ ──────────────────────────── ││
│                                           │ │ Body (optional):             ││
│                                           │ │                              ││
│                                           │ │ The token refresh was race   ││
│                                           │ │ condition when multiple reqs ││
│                                           │ │ arrived simultaneously.      ││
│                                           │ │                              ││
│                                           │ │ ──────────────────────────── ││
│                                           │ │ 47 chars ✓   [Commit] [Push] ││
│                                           │ └────────────────────────────────┘│
└──────────────────────────────────────────────────────────────────────────────┘
```

**The commit interface appears in context**, not as a modal dialog. You can still see and modify what's staged while writing your message.

**Commit Workflow:**
| Key | Action |
|-----|--------|
| `c` | Open commit interface (cursor in subject line) |
| `Tab` | Move between subject → body → staged files |
| `Ctrl+Enter` | Commit |
| `Ctrl+Shift+Enter` | Commit and push |
| `Esc` | Cancel (confirms if text entered) |

**Subject Line Helpers:**
- Character count: Green (<50), Yellow (50-72), Red (>72)
- If repo uses conventional commits: `feat:`, `fix:`, etc. suggested
- Ghost text shows similar recent commit subjects

**Amend Mode:**
- `ca` opens commit interface pre-filled with last commit message
- Staged files added to previous commit's changes
- Warning banner: "Amending will rewrite abc1234"
- Stronger warning if commit already pushed

### Push Interface

Press `p` to open push interface (replaces diff pane, like commit):

```
┌─ Push ───────────────────────────────────────────────────────────────────────┐
│                                                                              │
│  Branch: feature/auth                                                        │
│  Status: 3 commits ahead of origin/feature/auth                             │
│                                                                              │
│  ┌─ Commits to push ─────────────────────────────────────────────────────┐  │
│  │  ● abc1234 Fix authentication timeout — you, 5 min ago                │  │
│  │  ● def5678 Add token refresh logic — you, 1 hour ago                  │  │
│  │  ● ghi9012 Refactor auth module — you, 2 hours ago                    │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
│                                                                              │
│  Push to:                                                                    │
│    [✓] origin     → origin/feature/auth                                     │
│    [ ] upstream   → (will create upstream/feature/auth)                     │
│    [ ] personal   → personal/feature/auth (diverged ⚠)                      │
│                                                                              │
│  [ ] Force push (--force-with-lease)                                         │
│                                                                              │
│  [Push to 1 remote]  [Cancel]                                                │
└──────────────────────────────────────────────────────────────────────────────┘
```

**Push Interface Features:**
- Shows exactly which commits will be pushed
- Multiple remote selection with checkboxes
- Clear indication of what will happen to each remote
- Divergence warnings (remote has commits you don't have)
- Force push requires explicit checkbox, shows strong warning

**Remote Status Indicators:**
- `→ origin/branch` — will fast-forward
- `(will create)` — branch doesn't exist on remote yet
- `(diverged ⚠)` — remote has commits not in local, force required
- `(up to date)` — nothing to push (disabled)

**Quick Actions:**
| Key | Action |
|-----|--------|
| `p` | Open push interface |
| `P` | Quick push to default remote (no dialog, if safe) |
| `Space` | Toggle remote selection |
| `Enter` | Execute push |
| `Esc` | Cancel |

### Fetch & Pull

**Fetch** (`f`): Updates remote tracking branches without modifying working directory.

```
┌─ Fetch Results ─────────────────────────────────────────────┐
│                                                             │
│  origin:                                                    │
│    main         abc1234 → def5678  (+3 commits)            │
│    feature/x    (new branch)                                │
│                                                             │
│  upstream:                                                  │
│    main         (up to date)                                │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

**Pull** (`F`): Fetch + merge/rebase. Shows preview before executing:

```
┌─ Pull Preview ──────────────────────────────────────────────┐
│                                                             │
│  Pulling origin/main into main                              │
│  Strategy: rebase (configured)                              │
│                                                             │
│  Incoming commits (3):                                      │
│    ● def5678 Feature X — alice, 1 hour ago                 │
│    ● cba9876 Fix bug — bob, 2 hours ago                    │
│    ● xyz1234 Update deps — alice, 3 hours ago              │
│                                                             │
│  Your local commits (1) will be rebased on top:            │
│    ● abc1234 My local work — you, 30 min ago               │
│                                                             │
│  [Pull]  [Cancel]                                           │
└─────────────────────────────────────────────────────────────┘
```

### Multi-Repository Support

Whisper-git can track multiple working directories simultaneously.

**Use Cases:**
- Monorepo with multiple checkouts
- Main repo + submodule working directories
- Comparing two clones of same repo

**Repository Tabs:**
```
┌─────────────────────────────────────────────────────────────────────┐
│  [whisper-git]  [lib-crypto ●]  [lib-network]  [+]                 │
└─────────────────────────────────────────────────────────────────────┘
```

- Each tab is a separate repository
- Dot indicator shows repos with uncommitted changes
- `Ctrl+Tab` / `Ctrl+Shift+Tab`: Cycle repositories
- `Ctrl+1` through `Ctrl+9`: Jump to repository by position
- `+` or `Ctrl+O`: Open additional repository

**Cross-Repository View (Optional):**
For related repositories (e.g., main + submodules), option to view timelines aligned:

```
whisper-git    ●───●───●───●───●───●
                       ↓ updated lib-crypto
lib-crypto         ●───●───●───●
```

Shows when parent updated submodule reference relative to submodule's own history.

### Working Directory in Graph Context

The working directory appears as a special node above HEAD:

```
    ◐─ Working (3 files)     ← uncommitted changes
    │
    ●─ abc1234 Last commit — you, just now
    │
    ●─ def5678 Previous commit — alice, 2h ago
```

**Working Directory Node:**
- `◐` Half-filled circle (changes present) or `○` empty (clean)
- Shows file count
- Clicking it opens working directory panel
- Visually connected to HEAD (it's "where you are")

**Why Show It in the Graph:**
- Reinforces that uncommitted work is part of your position
- Makes it obvious when you have pending changes
- Natural place to click to start committing

---

## Navigation Model

### Navigation Philosophy: Snap, Don't Drift

**The problem with free pan/zoom:** Continuous scrolling and zooming feels modern but is often slower and more disorienting than discrete navigation. You lose your place. You overshoot. You spend effort on navigation instead of understanding.

**Our approach:** Snap between useful states. Every navigation action lands you somewhere meaningful—a commit, a branch, a marker. No drifting through empty space.

**This must be at least as fast as a tree view.** If pressing `j` ten times is slower than clicking ten rows in a list, we've failed.

### Discrete Zoom Levels

Four levels, nothing in between:

| Level | What You See | How You Get Here |
|-------|--------------|------------------|
| **Overview** | All markers, rope structure, submodule strip | `1` or `Ctrl+0` |
| **Topology** | Individual commits, short messages | `2` or `0` (default) |
| **Commit** | Full message, file list, diff preview | `Enter` on selected commit |
| **File** | Individual file history, inline diff | `Enter` on selected file |

**No continuous zoom.** Press a key, arrive at a level. The brief animated transition (150ms) shows spatial relationship, then you're there.

### Navigation Actions

**Primary: Keyboard (instant snapping)**

| Key | Action | Lands You At |
|-----|--------|--------------|
| `j` | Next commit | That commit, selected |
| `k` | Previous commit | That commit, selected |
| `J` | Next commit (skip 10) | That commit, selected |
| `K` | Previous commit (skip 10) | That commit, selected |
| `h` | Parent branch at merge | First commit on parent branch |
| `l` | Child branch at merge | First commit on child branch |
| `gg` | First commit | Oldest commit in view |
| `G` | Latest commit | HEAD |
| `{` | Previous merge | That merge commit |
| `}` | Next merge | That merge commit |
| `[` | Previous branch tip | That branch's HEAD |
| `]` | Next branch tip | That branch's HEAD |

Every action moves selection to a specific commit. No ambiguity.

**Secondary: Mouse (click targets, not drag)**

| Action | Result |
|--------|--------|
| Click commit | Select it (same as navigating to it) |
| Click branch label | Jump to that branch's HEAD |
| Click tag | Jump to tagged commit |
| Click in submodule strip | Expand that submodule |
| Scroll wheel | Move through commits (like `j`/`k`, not zoom) |

**Deliberate actions only:**
- Double-click commit: Open commit detail (zoom level 3)
- `Ctrl+scroll`: Zoom between levels (snaps to nearest level)

**What We Avoid:**
- Click-and-drag panning (disorienting, imprecise)
- Continuous zoom (lands you between useful states)
- Momentum scrolling (overshoots, requires correction)

**Quick Jump:**
- `/`: Search → select result → jump there
- `gb`: Go to branch (picker) → jump to branch HEAD
- `gt`: Go to tag (picker) → jump to tagged commit
- `gc`: Go to commit (enter SHA) → jump there

### Search System

Search is a first-class navigation tool, not a buried feature.

**Invocation:** `/` opens search overlay

**Search Targets:**
- Commits: By message, SHA, author
- Branches: By name
- Tags: By name
- Files: By path (shows commits touching that file)

**Search Behavior:**
- Fuzzy matching by default
- Results update as you type
- `Enter` jumps to first result
- Arrow keys navigate results
- `Esc` closes without jumping

**Search Syntax:**
- Plain text: Fuzzy search across all targets
- `@author`: Filter to commits by author
- `#branch`: Filter to specific branch
- `path:src/`: Filter to commits touching path
- Combinable: `@alice path:src/auth` (Alice's auth commits)

---

## Information Architecture

### Layer Model

Information is organized in layers, revealed progressively:

**Primary Layer (Always Visible)**
- Rope structure (main strand and parallel branches)
- All markers: branch tips, tags, remote tracking positions
- Current HEAD indicator (prominent)
- Submodule status strip
- Selected commit highlight

**Secondary Layer (On Hover/Select)**
- Commit message first line
- Author name/avatar
- Relative timestamp ("3 days ago")
- File change count badge (+5 -2)

**Tertiary Layer (On Expand)**
- Full commit message (all paragraphs)
- Complete file change list
- Per-file change indicators (+lines/-lines)
- Diff preview (first changed file)
- Parent/child commit links
- Associated branches and tags

### Information Density Philosophy

**Default to more, not less.** Power users want data visible, not hidden behind clicks.

**Commit Row (Topology View) Shows:**
```
● abc1234 Fix authentication timeout handling — alice, 3 days ago (feature/auth)
```

That's: indicator, SHA prefix, message, author, time, branch—all in one scannable line.

**What Requires Hover:**
- Full SHA (tooltip on prefix)
- Full timestamp (tooltip on relative time)
- File list (tooltip or expand)

**What Requires Click/Expand:**
- Full commit message body
- Diff content
- Related commits panel

---

## Interaction Patterns

### Selection Model

**Single Click:** Select commit
- Shows secondary layer info
- Commit becomes "focused" for keyboard navigation
- Other commits remain visible but de-emphasized

**Shift+Click:** Range select
- Selects all commits between current and clicked
- Useful for: comparing ranges, bulk operations
- Shows combined diff of range

**Ctrl+Click (Cmd+Click on Mac):** Multi-select
- Add/remove individual commits from selection
- Useful for: cherry-pick planning, comparing arbitrary commits
- Non-contiguous selection allowed

**Double-Click:** Zoom to commit
- Smooth zoom animation to commit detail level
- Commit centered in view
- Tertiary layer auto-expanded

### Context Menus

Right-click provides contextual actions without memorizing shortcuts.

**Commit Context Menu:**
- Copy SHA (full)
- Copy SHA (short)
- ---
- Checkout this commit
- Create branch here...
- Create tag here...
- ---
- Cherry-pick to current branch
- Revert this commit
- ---
- Show in file manager
- Copy commit message

**Branch Context Menu:**
- Checkout branch
- ---
- Merge into current...
- Rebase onto current...
- ---
- Rename branch...
- Delete branch
- ---
- Push to remote...
- Set upstream...

**File Context Menu (in commit detail):**
- Show file history
- Show blame
- ---
- Open in editor
- Open containing folder
- ---
- Copy path

### Keyboard Shortcuts Reference

**Navigation:**
| Key | Action |
|-----|--------|
| `j` | Next commit |
| `k` | Previous commit |
| `h` | Parent branch (at merge) |
| `l` | Child branch (at merge) |
| `gg` | First commit |
| `G` | Latest commit |
| `{` | Previous merge |
| `}` | Next merge |
| `/` | Search |

**Views (direct jump, no intermediate states):**
| Key | Action |
|-----|--------|
| `1` | Overview (all markers visible) |
| `2` | Topology (default, commit-level) |
| `3` | Timeline view |
| `4` | Contributor view |
| `Enter` | Commit detail (on selected) |
| `Esc` | Back up one level |

**Panels:**
| Key | Action |
|-----|--------|
| `b` | Toggle branches panel |
| `s` | Toggle stash panel |
| `r` | Toggle remotes panel |
| `?` | Show shortcuts help |

**Actions:**
| Key | Action |
|-----|--------|
| `Enter` | Expand selected commit |
| `Esc` | Collapse / deselect / close |
| `c` | Checkout selected |
| `y` | Copy SHA |
| `o` | Open commit in browser (if remote) |

**Command Palette:**
| Key | Action |
|-----|--------|
| `Ctrl+P` or `:` | Open command palette |

---

## Visual Design

### Color System

**Branch Colors (8-color palette):**
Automatically assigned, consistent within session:
1. Blue (primary—usually main/master)
2. Green
3. Orange
4. Purple
5. Cyan
6. Pink
7. Yellow
8. Red

Branches beyond 8 cycle with varied lightness.

**Semantic Colors:**
- Green `#4CAF50`: Additions, success states
- Red `#F44336`: Deletions, error states
- Blue `#2196F3`: Renamed/moved files
- Yellow `#FFC107`: Warnings, conflicts
- Gray `#9E9E9E`: Dimmed, inactive, metadata

**Commit States:**
- Normal: Branch color at 100% opacity
- Selected: Branch color with highlight ring
- Hovered: Branch color at 120% brightness
- Dimmed: Branch color at 30% opacity
- HEAD: Branch color with position indicator

### Typography

**Monospace (JetBrains Mono, Fira Code, or system):**
- SHA hashes
- File paths
- Code snippets in diffs
- Branch names

**Sans-serif (Inter, system-ui):**
- Commit messages
- Author names
- UI labels
- Timestamps

**Size Hierarchy:**
1. Branch names: 14px bold
2. Commit messages: 13px regular
3. Metadata (author, time): 12px regular
4. SHA prefix: 11px monospace

### Motion Design

**Principles:**
- Motion shows spatial relationship, then gets out of the way
- Transitions are brief (150ms max)—just enough to not be jarring
- Never block interaction; animation is skippable by pressing next key

**Level Transitions (Overview ↔ Topology ↔ Commit ↔ File):**
- Duration: 150ms
- Easing: ease-out
- Can be interrupted: pressing another key cancels and jumps immediately

**Navigation Within a Level:**
- Selection change: Instant (no animation)
- View recentering: 100ms slide to keep selection visible
- If selection is already visible: no movement at all

**What We Don't Animate:**
- Individual commit navigation (`j`/`k`) — instant
- Scrolling through commit list — instant
- Panel open/close — instant or very fast (50ms)

**Selection Feedback:**
- Highlight appears: Instant
- Previous selection unhighlights: Instant
- No fades, no transitions—immediate feedback

**Initial Load:**
- Rope structure + markers: Immediate (first frame)
- Commits populate: Next frame
- Ready for input: <100ms total

---

## Progressive Disclosure

### First Load Experience

**Sequence (total ~1s):**
1. **0-100ms:** Rope structure renders (main strand visible)
2. **100-300ms:** Markers appear (branch/tag labels populate)
3. **300-500ms:** Submodule strip populates
4. **500-700ms:** View centers on HEAD, parallel strands fade in
5. **700-1000ms:** Subtle hint appears: "Press ? for shortcuts"

**Goal:** User sees where HEAD is and where markers are positioned within 500ms.

**Initial Position:**
- View starts at topology zoom (not overview)
- Centered on HEAD commit
- Scrollable to see recent history without zooming

### Tooltip System

**Behavior:**
- Delay before show: 400ms (prevents flicker)
- Dismissable: Click anywhere or press Esc
- Remembers dismissal: Same tooltip won't show for 24h

**Content:**
- Brief, actionable information
- Keyboard shortcut hints where applicable
- Link to full documentation for complex features

### "Did You Know" System

Occasional, non-intrusive tips for power features:

- Show after user has used app 5+ times
- Maximum once per session
- Appears in non-blocking location (bottom-right)
- Dismisses automatically after 5 seconds
- Can be permanently disabled in settings

**Example Tips:**
- "Press / to search commits, branches, and files"
- "Shift+click to select a range of commits"
- "Press gg to jump to the first commit"

---

## Power User Considerations

### Information Density

**Philosophy:** Trust the user. Show information; don't hide it.

**What's Always Visible:**
- Commit SHA (prefix), message, author, date, branch
- All in one line, no truncation until necessary
- Hovering extends, doesn't replace

**What's One Click Away:**
- Full commit details (message body, files)
- Diff content
- Related commits

**What's Never Hidden Behind Multiple Clicks:**
- Any information about the currently selected commit
- Branch operations
- Navigation targets

### Keyboard-First Design

**Every action reachable via keyboard:**
- No mouse-only operations
- Shortcuts are primary, mouse is alternative
- Command palette (`:` or `Ctrl+P`) exposes all commands

**vim-style as primary, not alternative:**
- j/k/h/l are the expected navigation
- Arrow keys work too, but aren't advertised
- Muscle memory transfers from vim/less/more

**Command Palette:**
- Fuzzy search across all commands
- Shows keyboard shortcut next to each command
- Recently used commands sorted to top
- Accessible via `:` (vim-like) or `Ctrl+P` (VSCode-like)

### Performance Feel

**Responsiveness Targets:**
- Input response: <16ms (same frame)
- View update: <33ms (within 2 frames)
- Full render: <100ms (feels instant)

**Progressive Rendering:**
1. Structure first (branch lines) — immediate
2. Visible nodes — next frame
3. Off-screen nodes — background
4. Detailed metadata — on demand

**No Spinners for Local Operations:**
- Graph data comes from local git—it's fast
- If something takes >100ms, show partial results immediately
- Loading indicators only for network operations (fetch, push)

---

## Handling Complexity

### Multiple Remotes

Common in enterprise: origin (GitLab), upstream (GitHub), fork (personal).

**The Key Question:** Where is each remote's view of a branch relative to mine?

**Visualization Along the Rope:**
```
                    upstream/main   origin/main    HEAD (main)
                         ↓              ↓              ↓
════════════●════════════●══════════════●══════════════●
            ↑
         v2.0.0
```

This immediately answers:
- "Am I ahead of origin?" (yes, by 1 commit)
- "Is origin ahead of upstream?" (yes, by 2 commits)
- "Where was the last release?" (v2.0.0, 4 commits back)

**Marker Styling by Source:**
- Local branches: Solid background label
- Remote tracking (origin/*): Outlined label, origin icon
- Remote tracking (other): Outlined label, remote-specific icon
- Tags: Diamond/flag shape, distinct from branches

**Divergence Visualization:**
When local and remote have diverged (not just ahead/behind):
```
              origin/main
                   ↓
═══════●═════════●         (remote has commits we don't)
        ╲
         ●────●────●       (we have commits remote doesn't)
                   ↑
               HEAD (main)
```

**Fetch Status:**
- Last fetch timestamp in status bar
- Stale indicator (>1 hour) shows subtle warning icon
- `F` keyboard shortcut to fetch all remotes

### Submodules (Detailed Scenarios)

The submodule strip (see Core Views > Submodule Integration) handles common cases. Here are complex scenarios:

**Multiple Submodules (5+):**
- Strip scrolls horizontally if needed
- Group by status: problems first, then clean
- Keyboard: `m` cycles through submodules, `M` jumps to first problem

**Recursive Submodules:**
- Parent submodule shows nested count: `lib-core (2 nested)`
- Expand shows both the submodule and its children
- Full recursion available via double-click into context

**Submodule Update Commits:**
When a commit in the parent updates a submodule reference:
```
●─ abc123 Update lib-crypto to v2.0
│   └─ lib-crypto: fed098 → abc456 (+12 commits)
│      ├─ Fix buffer overflow
│      ├─ Add ChaCha20 support
│      └─ ... 10 more
```

Clicking the submodule change shows what commits were pulled in.

**Submodule Divergence:**
When your submodule checkout doesn't match the parent's expectation:
- Strip shows warning icon
- Tooltip explains: "lib-crypto is at xyz789 but parent expects abc456"
- Quick action: "Reset to expected" or "Stage current"

**Working in Submodules:**
- Changes in submodules show in parent's status
- Committing in submodule then committing reference update is a common flow
- Visual connection: submodule strip updates live as you work

### Many Branches (50+)

**Branch Grouping:**
- Automatic grouping by prefix: `feature/`, `bugfix/`, `release/`
- Collapsible groups in branches panel
- Group-level operations (hide all feature branches)

**Stale Branch Handling:**
- Stale threshold: Configurable (default 30 days)
- Stale branches dimmed by default
- "Hide stale" toggle (persisted preference)
- Stale branches excluded from branch color rotation

**Branch Search:**
- `/` search includes branches
- Dedicated branch search in branches panel
- Recent branches section (last 5 checked out)

### Large Repositories (1000+ commits visible)

**Level-of-Detail Rendering:**
- Overview: Rope collapses to density visualization, markers remain crisp
- Topology: Commit nodes visible, messages truncated aggressively
- Commit: Full detail for selected commit only

**Rope Compression:**
At overview zoom with many commits, the rope becomes a density visualization:
- Thick sections = many commits (high activity periods)
- Thin sections = sparse commits
- Markers (branches/tags) always rendered at full fidelity
- Click anywhere on the rope to zoom to that region

**Viewport Culling:**
- Only render what's visible + small buffer
- Off-screen commits unloaded from GPU
- Markers always kept in memory (small dataset)
- Instant re-render on pan

**Incremental Loading:**
- Load visible range + 200 commits buffer initially
- Load more as user scrolls/pans
- Background loading of full history
- Never block UI for history loading

---

## Appendix: Design Rationale

### Why "Frayed Rope" Not "Galaxy"?

Many git visualizers treat the commit graph as a sprawling network to explore. But most real repositories aren't galaxies—they're essentially linear with occasional parallel work. The interesting information isn't "what shape is this" but "where are things positioned":

- Where is my branch relative to origin?
- Where is the last release tag?
- How far ahead/behind am I?

The rope metaphor keeps position as the primary concept. Markers (branches, tags, remotes) are tied to specific points along the rope. You always know where you are.

### Why Not a Commit List?

Traditional git GUIs show a commit list with a graph decoration. This prioritizes chronology over topology. But chronology is misleading—commits from different branches interleave, making the actual structure invisible.

By making topology primary, we show what actually matters: where branches are positioned, how they relate, where they diverged. Time is metadata, not structure.

### Why vim Keys?

1. **Ergonomics:** Home row navigation is faster than arrow keys
2. **Ecosystem:** Developers often use vim bindings elsewhere
3. **No Conflicts:** Standard vim navigation doesn't conflict with OS shortcuts
4. **Power Scaling:** Simple keys for simple movement, composable for power use

### Why Semantic Zoom?

Showing all information at all zoom levels creates noise. Showing too little creates mystery. Semantic zoom means the right information appears at the right scale—branch names when seeing the whole repo, commit details when focused on one area.

### Why Snap Navigation Instead of Pan/Zoom?

Continuous pan and zoom feels "modern" but has real costs:

1. **Disorientation**: Where am I? Where was I? Smooth movement blurs these.
2. **Overshoot**: You zoom past your target, correct, overshoot again.
3. **Decision fatigue**: "How far should I zoom?" is a question you shouldn't have to answer.
4. **Speed**: Dragging to pan is slower than pressing `]` to jump to next branch.

A traditional tree view doesn't have these problems—click a row, you're there. We need to be at least that fast.

**Snap navigation means:**
- Press a key, arrive somewhere specific
- Four zoom levels, not infinite gradations
- Scroll wheel moves through commits, not zooms
- Every action has a predictable destination

### Why Not Loading Spinners?

Local git operations are inherently fast (milliseconds). If we show spinners, we're either:
1. Blocking on something we shouldn't be
2. Creating false perception of slowness

Instead: show partial results immediately, complete in background. The user sees *something* useful instantly.

### Why Is the Submodule Strip Always Visible?

Submodule state is easy to forget and painful to debug later. By keeping it visible:
1. You notice mismatches before they become problems
2. The cost of a submodule update is obvious (see the commit count)
3. You can't accidentally commit with wrong submodule state

It's a small amount of screen space for a large reduction in "why doesn't this build?" moments.

---

## Document History

| Version | Date | Changes |
|---------|------|---------|
| 0.1 | 2025-02 | Initial UX design document |
| 0.2 | 2025-02 | Replace "Galaxy" with "Frayed Rope" metaphor; emphasize marker positions, submodule strip visibility, remote/local divergence |
| 0.3 | 2025-02 | Replace continuous pan/zoom with snap navigation; discrete zoom levels; instant selection feedback |
| 0.4 | 2025-02 | Add editing capabilities: working directory as primary view, two-pane file/diff layout, commit/push/pull interfaces, multi-repository support |
