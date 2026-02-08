# Whisper-Git User Needs

**Captured:** February 2026 (from initial design sessions)

---

## Core Information Needs (Priority Order)

1. **What does the recent commit graph look like?**
   - Where are branches and tags?
   - What's the topology of recent work?

2. **What are my working directories, and how are they dirty?**
   - Submodule status at a glance
   - Worktree status at a glance

3. **What am I working on committing right now?**
   - Staged vs unstaged files
   - Current commit message draft

---

## Key Actions

- Staging/unstaging files
- Committing
- Pushing to one or more remotes
- Fetching
- Coordinating merges and rebases
- Inspecting changes from individual commits

---

## Design Constraints

| Constraint | Rationale |
|------------|-----------|
| Mouse-first, vim-optional | "Mouse and scroll should be all that is needed to navigate and control" |
| Rich Vulkan GUI | Not a CLI/TUI - take advantage of GPU rendering |
| Multiple repositories in parallel | "See at a glance what the various submodules are doing" |
| Primary + secondary repo model | One repo as primary focus, secondary repos can be focused when needed |
| Information-dense | Maximize useful information without wasting screen space |

---

## Visual Requirements

### Commit Size Indication
Small commits should be visually distinct from large commits. Show `+N/-M` line counts inline.

### LLM-Generated Descriptions
Use a cheap LLM (Claude Haiku or local model) to generate abbreviated descriptions of changesets. Each working tree or dirty directory can get a quick tagline explaining the changes.

---

## Technology Decisions

- **Vulkano** for rendering (custom pipeline, no egui)
- **Custom font/widget system** (no external UI framework)
- **git2-rs** for git operations
- **Native-only** (no WASM compatibility needed)

---

## Feature Priorities

### Top Priority (Lesser-Known Git Features)
- Worktrees
- Submodules

### Standard Features
- Commit graph visualization
- Branch/tag management
- Staging workflow
- Push/pull/fetch operations

### Deferred
- Merge conflict resolution
- Interactive rebase
- Pull request integration
