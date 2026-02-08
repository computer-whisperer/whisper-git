# Whisper-Git Design Feedback

**Date:** February 2026
**Status:** Design review consolidation for next iteration
**Reviewers:** Information Architect, Power User Workflow, Visual Systems, Product Strategy

---

## Executive Summary

Whisper-Git has a solid technical foundation (custom Vulkan rendering, proper text handling, spline-based graph) but currently uses that power to recreate the same generic git client aesthetic. The application is approaching "just another git client" - functional but not special.

**Core problem:** The design treats worktrees and submodules as secondary features when they should be the primary differentiator.

**Strategic recommendation:** Pivot from "repository state viewer" to "work context navigator for multi-repo projects."

---

## Current State Assessment

### Screenshots Analyzed
- Two repositories tested: whisper-git (16 commits) and Raven-Firmware (50 commits, 5 submodules)
- Full 1920x1080 views plus cropped regions (header, graph, staging, secondary repos)

### What Works
- Commit graph topology rendering with colored lanes
- Clean dark theme with proper elevation hierarchy
- Bezier curve connections between commits
- Basic staging workflow (stage/unstage/commit)
- Secondary repos panel showing worktrees and submodules

### Critical Gaps

| Gap | Impact |
|-----|--------|
| No commit size visualization (+N/-M lines) | Can't distinguish typo fix from major refactor |
| No commit age/recency | Can't see activity patterns |
| No author information | Can't track who worked on what |
| No ahead/behind per branch | Only shown in header for current branch |
| No dirty indicators on secondary repos | Defeats the purpose of showing them |
| ~40% wasted screen space | Empty state boxes, sparse header, underutilized secondary panel |

### Priority Inversion Problem

User requirements state: Graph (P1) > Submodules/Worktrees (P2) > Staging (P3)

Current UI shows: Staging dominates attention, Graph is plain text, Secondary repos buried at bottom

---

## Information Density Audit

### Currently Shown vs Missing

```
COMMIT ROW - CURRENT:
  0dd9b30 Add SecondaryReposView for submodules and worktrees

COMMIT ROW - PROPOSED:
  0dd9b30 Add SecondaryReposView...    +247/-89  2h ago  cdw
          ^^^^^^                        ^^^^^^   ^^^^^^  ^^^
          hash                          delta    age     author
```

### Wasted Space Analysis

**Header bar:** 80% empty gray space. Should show tracking status, last fetch time, working directory summary.

**Empty staging states:** "No staged changes" consumes ~80px for a single line of text. Should collapse to single line.

**Secondary repos panel:** One worktree card consumes same space as 5 submodule cards. Massive empty space below.

### Proposed Information Additions

1. Inline commit metadata (size, age, author) - right-aligned, dimmed
2. Submodule expected vs actual commit status
3. Branch ahead/behind counts in graph
4. Last fetch timestamp in header
5. Remote tracking relationship display

---

## Hierarchy and Layout Recommendations

### Screen Layout Restructure

Current layout prioritizes staging over the stated priorities. Proposed adjustment:

```
CURRENT LAYOUT:
+------------------+------------------+
|                  |    Staging       |
|    Graph         |    (45% x 45%)   |
|    (55%)         +------------------+
|                  |  Secondary Repos |
|                  |    (45% x 51%)   |
+------------------+------------------+

PROPOSED LAYOUT (Option A - Elevate Secondary):
+------------------+------------------+
|                  | Secondary Repos  |
|    Graph         |    (45% x 40%)   |
|    (55%)         +------------------+
|                  |    Staging       |
|                  |    (45% x 56%)   |
+------------------+------------------+

PROPOSED LAYOUT (Option B - Integrated Secondary):
+----------------------------------------+
| [Worktree tabs: main | feature | hotfix]|
+------------------+---------------------+
|                  |                     |
|    Graph         |    Staging          |
|    (55%)         |    (45%)            |
|                  |                     |
+------------------+---------------------+
| Submodule status strip (single row)    |
+----------------------------------------+
```

### Header Bar Enhancement

```
CURRENT:
+------------------------------------------------------------+
| main | main                          Fetch  Push(+6) Commit |
+------------------------------------------------------------+

PROPOSED:
+------------------------------------------------------------+
| whisper-git/main  <- origin/main (even)  | 2 unstaged | Fetch |
| Last fetch: 4m ago   Remotes: origin ok  | 0 staged   | Push+6|
+------------------------------------------------------------+
```

Two-line header showing: tracking relationship, sync status, working directory summary, last fetch age.

---

## Commit Graph Enhancements

### Size Visualization

Commits should have variable visual weight based on lines changed:

```
Node radius scaling:
  Small (1-10 lines):      5px
  Medium (10-100 lines):   8px
  Large (100-500 lines):   12px
  Massive (500+ lines):    16px

Additional: dual-tone fill showing +/- ratio
  - All green = pure addition
  - All red = pure deletion
  - Mixed = refactoring
```

### Time Density

Row spacing should reflect time gaps (logarithmic):
- Commits within 1 hour: 20px (dense clustering)
- Commits within 1 day: 34px (normal)
- Commits within 1 week: 50px (expanded)
- Older: 28px (compressed)

Creates visual "bursts" of activity.

### Author Strips

4px vertical color strip on left edge of each commit row, colored by author hash. Shows ownership patterns and handoffs at a glance.

### Branch Health

Branch line styling based on staleness:
- Active (< 1 week): Full opacity, 3px width
- Aging (1-4 weeks): 80% opacity, 2.5px width
- Stale (> 1 month): 50% opacity, 2px width, dashed
- Abandoned (> 3 months): 30% opacity, 1.5px width, dashed

---

## Secondary Repos Redesign

### Current Problems
- Verbose card format (2 lines per repo)
- No dirty/sync status indicators
- No expected vs actual commit comparison for submodules
- Massive empty space when few repos

### Proposed: Status Strip Format

```
+-- WORKTREES ---------------------------------------------+
| main        @ main      clean              [*]          |
| feature-x   @ feature   +2 dirty           [!]          |
+---------------------------------------------------------+
+-- SUBMODULES (5) ----------------------------------------+
| Cushion-Controller  @ HEAD      DETACHED   [ ]          |
| embassy             @ raven_m4  +3 dirty   [!]          |
| nanoarrow-rs        @ main      clean      [ ]          |
| oggopus-embedded    @ main      clean      [ ]          |
| trouble             @ srv_uuid  +1 staged  [~]          |
+---------------------------------------------------------+

Legend: [*]=current [!]=dirty [~]=staged [ ]=clean
```

One line per repo. Status indicators. Click to focus.

### Proposed: Constellation View (Advanced)

Visualize submodule relationships and divergence:

```
+-- CONSTELLATION -----------------------------------------+
|                                                          |
|   whisper-git/main --*---------------* HEAD (2 dirty)    |
|                      |                                   |
|   +- embassy --------+--*------------* raven_m4 (+3)     |
|   |                     ^                                |
|   |                     +2 ahead of pinned               |
|   |                                                      |
|   +- nanoarrow ------*---------------* main (clean)      |
|   |                                                      |
|   +- trouble --------*------*--------* srv_uuid (+1)     |
|                             ^                            |
|                      pinned here, remote has +4          |
|                                                          |
+----------------------------------------------------------+
```

Shows: submodule position relative to parent's pinned commit, dirty state, remote divergence.

---

## Worktree-Centric Paradigm

### The Shift

```
CURRENT MODEL:
  Repository -> has Worktrees (secondary)

PROPOSED MODEL:
  Worktree Collection -> each is first-class workspace
  Repository is metadata connecting them
```

### Worktree Workspace View

```
+-- WORKSPACES ------------------------------------[+ New]+
| +---------------+ +---------------+ +---------------+   |
| | main          | | feature/auth  | | hotfix/crash  |   |
| | @ main        | | @ feature/auth| | @ hotfix/crash|   |
| | ~~~~~~~~~~~~~ | | ~~~~~~~~~~~~~ | | ~~~~~~~~~~~~~ |   |
| | o Update docs | | o Add OAuth   | | * Fix crash   |   |
| | o Refactor API| | o Add tests   | |   [WIP]       |   |
| |               | |               | |               |   |
| |   CLEAN       | |   CLEAN       | |   2 DIRTY     |   |
| +---------------+ +---------------+ +---------------+   |
|                                                         |
| [Double-click to focus] [Drag to compare]               |
+---------------------------------------------------------+
```

Each worktree gets a mini-graph preview. Click to focus. Drag between to compare.

---

## Parallel Branch Comparison

### The View

Side-by-side branch comparison showing divergence point and unique commits:

```
+-- COMPARING: main <-> feature/async-refactor ---[Swap][X]+
+----------------------------+-----------------------------+
|           main             |   feature/async-refactor    |
| ========================== | =========================== |
|                            |                             |
|  o abc123 Update docs <----+---- (common ancestor)       |
|  |                         |  |                          |
|  o def456 Fix API          |  o 111aaa Convert to async  |
|  |                         |  |                          |
|  o ghi789 Add validation ==+==o 222bbb Add tokio         |
|  |                    ^    |  |                          |
|  o jkl012 Refactor  MERGE  |  o 333ccc Refactor pool     |
|  |                  POINT  |  |                          |
|                            |  o 444ddd Add retry         |
|                            |  |                          |
|  - - - - - - - - - -       |  o 555eee WIP: Testing      |
|                            |                             |
+----------------------------+-----------------------------+
| DIVERGENCE: main +4 behind, feature +5 unique commits    |
| MERGE PREVIEW: 5 commits, 23 files (+1,847 / -234)       |
|                                                          |
| [Preview Merge] [Preview Rebase] [Cherry-pick] [Create PR]|
+----------------------------------------------------------+
```

### Key Features
- Common ancestor highlighting with horizontal connector
- Cherry-pick detection (ghost commits if duplicated)
- Conflict prediction with file-level warnings
- Interactive: drag commits for cherry-pick, click for merge preview

---

## GPU-Native Visual Opportunities

### Animations (60fps, minimal CPU)

1. **Working directory pulse** - Dirty indicator breathes; rate increases with dirty file count
2. **Branch breathing** - Active branches oscillate width subtly (2-4s cycle)
3. **Selection morphing** - Smooth transitions instead of snaps
4. **Momentum scrolling** - Smooth scroll with bounce at edges

### Shader Effects

1. **HEAD glow** - Radial falloff shader instead of layered circles
2. **Heat map mode** - Commit density as background gradient
3. **Anti-aliased curves** - Distance-field Bezier for perfect curves at any zoom

### Compute Shaders

1. **Force-directed layout** - For complex merge histories
2. **Lane assignment** - Parallel processing for large repos
3. **Real-time re-layout** - During interaction without frame drops

---

## Three Visual Signatures

To make Whisper-Git instantly recognizable:

### Signature 1: "The Pulse"

Working directory indicator pulses when dirty. Pulse rate scales with dirty file count:
- 1-5 files: 2s period (gentle)
- 6-20 files: 1s period (medium)
- 20+ files: 0.5s period (urgent, color shifts to orange)

Creates subconscious awareness of commit hygiene.

### Signature 2: "The Comet"

Commit nodes have tapered gradient "tails" pointing to parent. Tail length/brightness scales with commit size:
- Small commits: short, dim tail (distant star)
- Large commits: long, bright tail (comet)
- Massive merges: particle-like dispersion effect

Every screenshot shows this unique visual language.

### Signature 3: "The Breath"

Active branches (commits < 7 days) have lines that subtly oscillate width. Breath rate correlates with recency:
- Commit today: 2s cycle (faster)
- Commit this week: 4s cycle (slower)
- Stale: static (no animation)

Branches feel "alive" when worked on, "dormant" when neglected.

---

## The 10x Feature: Project Rewind

### Concept

Every 30 seconds, silently capture:
- Working directory status of all tracked repos
- Branch positions
- Stash contents
- Index state

Users can scrub through time to see past states:

```
+-- PROJECT STATE TIMELINE -------------------------[3h]+
|                                                       |
|  [*]--[*]--[*]--[*]--[*]--[*]--[*]--[*]--[*]--[NOW]  |
|   |                   |                               |
|   |                   +-- "Started auth work"         |
|   +-- "Fresh after pull"                              |
|                                                       |
|  Scrubbing to: 45 minutes ago                         |
|  +------------------------------------------------+  |
|  | main-app/     dirty(2)  auth.rs, config.rs     |  |
|  | lib-ui/       clean                            |  |
|  | lib-crypto/   dirty(1)  hash.rs                |  |
|  +------------------------------------------------+  |
|                                                       |
|  [Restore This State] [View Diff to Now] [Continue]   |
|                                                       |
+-------------------------------------------------------+
```

### Why This Is 10x

1. **Eliminates context loss** - #1 productivity killer for multi-context developers
2. **Zero discipline required** - No manual saves, commits, or tags
3. **Impossible in web apps** - Requires local FS access and fast storage
4. **GPU makes it beautiful** - Smooth timeline scrubbing with instant preview
5. **Debugging superpower** - "When did this file start looking wrong?"

### Implementation Notes

- Store state snapshots in local SQLite (timestamps, file hashes, dirty states)
- Don't store file contents (too large) - store working tree state + index state
- "Restore" generates git commands (checkout, stash pop, etc.)
- GPU rendering makes timeline feel magical vs loading spinners

---

## LLM Integration Opportunities

### Beyond Commit Message Suggestions

| Feature | Value |
|---------|-------|
| Branch intent summarization | "What is this branch FOR?" not just "what files changed" |
| Wrong-branch detection | "These changes look like feature work, not hotfix" |
| Divergence explanation | "Why did main and feature diverge? Here's what each received" |
| Multi-repo coordination | "Commit lib-crypto first, then update dependents, here's the sequence" |
| Auto-tagging | Classify commits as FEATURE/BUGFIX/REFACTOR/CONFIG without user input |
| Context restoration | "Welcome back! Here's where you left off yesterday at 5:32 PM" |

### Priority: Multi-Repo Coordination

Guide users through cross-repo commits:

```
You staged changes in lib-crypto/
Dependent repos: main-app, backend-service

Suggested commit order:
1. Commit lib-crypto (version bump to 2.1.0)
2. Update main-app/Cargo.toml to reference 2.1.0
3. Update backend-service/Cargo.toml to reference 2.1.0
4. Commit both dependents

[Execute Sequence] [Customize]
```

---

## Interaction Model Improvements

### Right-Click Context Menus

**On commit node:**
- Checkout this commit
- Create branch here...
- Create worktree here...
- Cherry-pick to... -> [branch list]
- Revert this commit
- Copy commit hash / message

**On branch label:**
- Checkout
- Open in new worktree
- Compare with... -> [branch list]
- Merge into current
- Rebase onto current
- Push / Delete / Rename

**On submodule card:**
- Open in primary view
- Update to expected commit
- Update to latest upstream
- Show diff from expected
- Stage pointer change

### Command Palette (Cmd/Ctrl+P)

```
+-- > _ -----------------------------------------------+
| Recent:                                              |
|   commit              Create commit                  |
|   push                Push to origin                 |
|   checkout main       Checkout main branch           |
| ---------------------------------------------------- |
| Suggestions:                                         |
|   compare...          Compare two branches           |
|   worktree new        Create new worktree            |
|   submodule update    Update all submodules          |
+------------------------------------------------------+
```

### Selection Model

Expand from single commit to rich selection:

```rust
pub enum Selection {
    None,
    SingleCommit(Oid),
    CommitRange { from: Oid, to: Oid },
    MultipleBranches(Vec<String>),
    MultipleWorktrees(Vec<String>),
}
```

Enables: Shift+click range, Ctrl+click multi-select, operations on selection.

### Drag-and-Drop Grammar

| Drag Source | Drop Target | Action |
|-------------|-------------|--------|
| Commit node | Branch label | Cherry-pick to branch |
| Commit node | Worktree card | Cherry-pick to worktree |
| Branch label | Branch label | Merge source into target |
| File (staging) | Commit | Amend file into commit |
| Submodule card | Main view | Focus submodule |

---

## Target User Profile

### The Systems Architect

**Work environment:**
- Maintains 3-15 repositories forming a coherent system
- Uses worktrees for parallel feature development
- Has submodules for shared libraries or vendored dependencies
- Context-switches between repos throughout the day
- Often works on changes spanning multiple repositories

**Daily pain points:**
1. "Which worktree was I working on that API change?" (Lost context)
2. "Did I push the submodule before updating the parent?" (Dependency ordering)
3. "Three repos are dirty - which ones matter for this feature?" (Attention fragmentation)
4. "What was the state of everything when I left Friday?" (Temporal context loss)

**Why existing tools fail:**
- Git clients are repository-scoped
- Cannot answer "What's happening across my whole project?"
- Cannot answer "Where was I working an hour ago?"
- Cannot answer "Which changes belong together across repos?"

---

## Strategic Positioning

### Avoid Competing With

| Tool | Their Space |
|------|-------------|
| GitKraken/SourceTree | "Friendly git GUI for everyone" |
| VSCode Source Control | Integrated-into-editor git |
| GitHub Desktop | Single-repo open-source contribution |
| Git CLI | Power users who prefer text |

### Own This Space

**"The cockpit for polyrepo/multi-context development"**

Not a git client that happens to show submodules, but a tool that treats your entire project ecosystem as the fundamental unit.

### The One-Liner

**"Whisper-Git: The first git client that remembers what you were doing."**

---

## Implementation Priority

### Phase 1: Information Density (Low effort, high impact)
- [ ] Add +N/-M line counts to commit rows
- [ ] Add commit age (relative time)
- [ ] Add author initials/colors
- [ ] Collapse empty staging states
- [ ] Add dirty indicators to secondary repo cards

### Phase 2: Secondary Repos Elevation (Medium effort, high impact)
- [ ] Redesign to status strip format (one line per repo)
- [ ] Add expected vs actual for submodules
- [ ] Make cards clickable to focus
- [ ] Add submodule batch operations (update all, fetch all)

### Phase 3: Visual Signatures (Medium effort, high differentiation)
- [ ] Implement variable commit node sizes
- [ ] Add "The Pulse" for dirty working directory
- [ ] Add branch line staleness visualization
- [ ] Implement "The Breath" for active branches

### Phase 4: Paradigm Features (High effort, transformative)
- [ ] Project Rewind / Time Machine
- [ ] Parallel branch comparison view
- [ ] Worktree-centric main view
- [ ] Constellation view for submodules

### Phase 5: Intelligence (High effort, competitive moat)
- [ ] LLM multi-repo coordination
- [ ] Branch intent summarization
- [ ] Context restoration on app launch
- [ ] Wrong-branch detection

---

## Appendix: Competitor Gaps

| Feature | GitKraken | Fork | SourceTree | Whisper-Git Opportunity |
|---------|-----------|------|------------|-------------------------|
| Worktree management | Hidden in menus | Decent | Poor | Make PRIMARY |
| Submodule batch ops | Per-submodule only | Limited | Limited | Matrix view + batch |
| Parallel branch view | Tabs only | Tabs only | Tabs only | True side-by-side |
| Commit size viz | None | None | None | Visual encoding |
| Time machine | None | None | None | Unique feature |
| Multi-repo coordination | Separate windows | Separate windows | Separate tabs | Unified view |

---

## References

- User requirements: `docs/user_needs.md`
- Current layout: `src/ui/layout/screen.rs`
- Commit graph: `src/views/commit_graph.rs`
- Secondary repos: `src/views/secondary_repos.rs`
- Staging well: `src/views/staging_well.rs`
- Theme colors: `src/ui/widget.rs` (lines 222-253)
