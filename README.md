# Whisper Git

A GPU-accelerated Git client built in Rust with Vulkan. Designed for power users who want fast, visual interaction with Git repositories, including support for worktrees and submodules.

## Features

### Rendering Engine
- **GPU-accelerated via Vulkan** using vulkano 0.35 -- no egui, no immediate-mode GUI frameworks
- **Custom text rendering** with a font atlas built from Roboto Regular (proportional) via ab_glyph
- **Spline rendering** for commit graph connections using CPU-tessellated Bezier curves
- **Avatar rendering** with a dedicated GPU atlas for Gravatar images
- **Three-layer render pipeline**: graph layer → chrome layer → overlay layer, each with splines → avatars → text passes
- **Dark blue-gray theme** (#1a1e24 family) with rounded corners, pill outlines, and drop shadows
- **HiDPI/4K aware** -- all layouts scale with the display scale factor
- **Continuous redraw** for smooth cursor blink and toast animations

### Commit Graph
- Branch topology visualization with colored lane assignments
- Lane reuse algorithm that keeps the graph compact (3-4 lanes typical)
- Smooth S-curve Bezier merge/fork connection lines
- 24-segment circle nodes at each commit
- Each row displays: graph lanes | commit subject | author (dimmed) | relative time (right-aligned)
- Color-coded pill labels at branch tips: local (blue), remote (cyan), tag (amber), HEAD (green) with outlines
- Time-based variable row spacing (log-scaled gaps between commit groups)
- Zebra striping and row hover highlighting
- Graph column shadow on right edge of lane area
- Click to select a commit and view its diff
- Infinite scroll -- loads more commits as you scroll down
- Working directory status row at the top when changes exist
- Right-click context menu: Copy SHA, View Details, Checkout, Create Branch, Create Tag, Cherry-pick, Revert, Reset (Soft/Mixed/Hard)

### Branch Sidebar
- Six collapsible sections: LOCAL, REMOTE, TAGS, SUBMODULES, WORKTREES, STASHES
- Rounded section headers with collapse arrows and item count badges
- Current branch highlighted with accent color and left stripe
- Keyboard navigation (j/k, PageUp/PageDown) and selection (Enter to checkout)
- Delete branches with `d`, create tags from context menu
- Right-click context menus: Checkout, Delete, Push, Merge into Current, Rebase Current onto
- Submodule context menu: Open in Terminal, Update, Delete
- Worktree context menu: Open in Terminal, Jump to Branch, Remove
- Stash context menu: Apply, Pop, Drop
- Scrollbar with proportional thumb and auto-scroll on keyboard navigation

### Staging Area
- Split view: commit message editor (subject + body) and file lists
- Subject line input with 72-character limit and placeholder text
- Multi-line body text area
- Separate staged and unstaged file lists with styled headers and colored status indicators
- Stage/unstage individual files (double-click) or all files at once
- Click files to view their diff
- Ctrl+Enter to commit, amend mode (Ctrl+Shift+A) pre-fills from last commit
- Right-click context menu (Stage, Unstage, View Diff, Discard)
- Auto-focus subject input after successful commit

### Diff Viewer
- Color-coded diff display (green additions, red deletions, purple hunk headers)
- **Word-level diff highlighting** -- changed portions within lines get a brighter background
- **Horizontal scrolling** with Shift+ScrollWheel or Left/Right arrow keys
- **Hunk-level staging** -- Stage/Unstage buttons on each hunk header
- Line numbers in the gutter
- Supports both commit diffs and working directory diffs (staged and unstaged)

### Commit Detail Panel
- Full commit metadata: SHA, author, date, parent commits, full message
- Clickable file list with +/- addition/deletion stats per file
- Select a file to view its individual diff

### Remote Operations
- **Fetch**, **Pull**, and **Push** buttons in the header bar
- Operations run in background threads (non-blocking UI)
- Button labels show status: "..." while in progress, "Pull (-N)" when behind, "Push (+N)" when ahead
- Toast notifications on success or failure
- Automatic UI refresh after remote operations complete

### Multi-Repository Tabs
- Open multiple repositories as tabs (Ctrl+O to open, Ctrl+W to close)
- Tab bar with close buttons, click to switch
- Ctrl+Tab / Ctrl+Shift+Tab to cycle between tabs
- Each tab maintains independent view state (focused panel, scroll positions, selections)
- Toast notification when opening/closing tabs

### Gravatar Avatars
- Automatic download of Gravatar images based on author email (md5 hash)
- 512x512 GPU atlas with 64 avatar slots (64x64 each)
- Disk cache at `~/.cache/whisper-git/avatars/`
- Falls back to colored identicon (first initial with deterministic color) when no Gravatar exists
- Togglable in Settings

### Context Menus
- Right-click context menus on commit graph, branch sidebar, and staging area
- Keyboard navigation (j/k or Up/Down, Enter or Space to select)
- Separator support for visual grouping
- Click outside or press Escape to close
- Shadow and border styling

### Toast Notifications
- Color-coded notifications: green (Success), red (Error), blue (Info)
- Auto-dismiss after 4 seconds with 1-second fade-out
- Stacking (up to 3 visible at once)
- Bottom-center overlay positioning

### Search / Filter
- Activate with Ctrl+F or `/` in the commit graph
- Matches against commit subject, author name, and SHA
- Non-matching commits are dimmed (not hidden)
- Blinking cursor in search input

### Settings Dialog
- Modal overlay triggered from the header bar Settings button
- **Show Avatars**: toggle Gravatar avatar display on/off
- **Scroll Speed**: Normal or Fast (2x)
- **Row Size**: Normal or Large (1.5x)
- Settings persisted to `~/.config/whisper-git/settings.json`
- Escape or click outside to close

### Git Operations
- **Commit**: Stage files, write message, Ctrl+Enter to commit
- **Amend Commit**: Ctrl+Shift+A toggles amend mode, pre-fills subject/body from HEAD
- **Revert Commit**: Context menu with confirmation dialog
- **Reset to Commit**: Soft, Mixed, or Hard reset with confirmation (Hard has strong warning)
- **Create/Delete Branch**: From graph or sidebar context menus with name dialog
- **Create/Delete Tag**: From graph or sidebar context menus with name dialog
- **Merge/Rebase**: Sidebar context menu "Merge into Current" / "Rebase Current onto"
- **Cherry-pick**: Graph context menu with confirmation
- **Stash**: Ctrl+S push, Ctrl+Shift+S pop, sidebar context menu for Apply/Pop/Drop
- **Clipboard**: Copy SHA to system clipboard via arboard

### UI Polish
- **Rounded corners** on buttons, pills, inputs, dialogs, context menus, toasts
- Panel backgrounds with 1px borders for depth separation, drag-resizable dividers
- Focused panel indicator (2px accent-colored top border)
- Context-sensitive shortcut bar with key pills below the header (toggleable)
- Header bar with branch name pill, primary Commit button, ghost secondary buttons, drop shadow
- Scrollbars with proportional thumb, drag support, and accent hover color
- Hover highlighting on buttons, sidebar items, file list items, and graph rows
- Cursor blinking in text inputs at approximately 1Hz, accent border on focus
- Empty state messages ("Working tree clean" with a checkmark)
- Colored file status dots (green for Added, yellow for Modified, red for Deleted)
- Confirmation dialogs for all destructive operations

## Keyboard Shortcuts

### Global
| Key | Action |
|-----|--------|
| `Ctrl+O` | Open a new repository tab |
| `Ctrl+W` | Close the current tab |
| `Ctrl+Tab` | Switch to the next tab |
| `Ctrl+Shift+Tab` | Switch to the previous tab |
| `Tab` | Cycle focus: Graph -> Staging -> Sidebar -> Graph |
| `Ctrl+S` | Stash push (when text input not focused) |
| `Ctrl+Shift+S` | Stash pop |
| `Escape` | Close diff view, then commit detail, then exit |

### Commit Graph (when focused)
| Key | Action |
|-----|--------|
| `j` / `k` | Navigate commits up/down |
| `PageUp` / `PageDown` | Navigate commits by page |
| `Enter` | Select commit (view details and diff) |
| `Ctrl+F` or `/` | Open search/filter bar |
| Right-click | Context menu: Copy SHA, View Details, Checkout, Create Branch, Create Tag, Cherry-pick, Revert, Reset |

### Branch Sidebar (when focused)
| Key | Action |
|-----|--------|
| `j` / `k` | Navigate items up/down (with auto-scroll) |
| `PageUp` / `PageDown` | Navigate by page |
| `Enter` | Checkout branch / Apply stash / Jump to worktree branch |
| `d` | Delete the selected branch |
| Right-click | Context menu varies by item type |

### Staging Area (when focused)
| Key | Action |
|-----|--------|
| `Tab` | Cycle between subject, body, staged list, unstaged list |
| `Ctrl+Enter` | Create commit with current message |
| `Ctrl+Shift+A` | Toggle amend mode |
| Double-click file | Stage/unstage individual file |
| Right-click | Context menu: Stage/Unstage, View Diff, Discard |

### Diff Viewer
| Key | Action |
|-----|--------|
| `Shift+ScrollWheel` | Horizontal scroll |
| `Left` / `Right` | Horizontal scroll |
| `ScrollWheel` | Vertical scroll |

### Context Menus (when open)
| Key | Action |
|-----|--------|
| `j` / `Down` | Move selection down |
| `k` / `Up` | Move selection up |
| `Enter` / `Space` | Activate selected item |
| `Escape` | Close menu |

## Building

```bash
cd main
cargo build --release
```

### Requirements
- Rust 2024 edition
- Vulkan-capable GPU with drivers installed
- System font: `/usr/share/fonts/TTF/Roboto-Regular.ttf`
- Linux (tested on Arch Linux)

### Dependencies
| Crate | Version | Purpose |
|-------|---------|---------|
| vulkano | 0.35 | Vulkan rendering |
| vulkano-shaders | 0.35 | GLSL shader compilation |
| winit | 0.30 | Window creation and event loop |
| git2 | 0.20 | Git operations (libgit2 bindings) |
| ab_glyph | 0.2 | Font rasterization for atlas |
| image | 0.25 | PNG/JPEG encoding for screenshots and avatars |
| ureq | 2 | HTTP client for Gravatar downloads |
| md5 | 0.7 | MD5 hashing for Gravatar email lookup |
| bytemuck | 1.21 | Safe transmute for vertex data |
| anyhow | 1.0 | Error handling |
| half | 2.4 | f16 conversion for AMD swapchain formats |
| serde / serde_json | 1 | Settings persistence to JSON |
| arboard | 3 | System clipboard access |

## Usage

```bash
# Open the current directory
cargo run

# Open a specific repository
cargo run -- /path/to/repo

# Open multiple repositories (one tab each)
cargo run -- /path/to/repo1 /path/to/repo2

# Screenshot mode (renders and captures to PNG, then exits)
cargo run -- --screenshot output.png

# Screenshot at specific resolution (offscreen rendering)
cargo run -- --screenshot output.png --size 1920x1080
cargo run -- --screenshot output.png --size 3840x2160

# Screenshot with a specific UI state
cargo run -- --screenshot output.png --screenshot-state search
cargo run -- --screenshot output.png --screenshot-state context-menu
cargo run -- --screenshot output.png --screenshot-state commit-detail
cargo run -- --screenshot output.png --screenshot-state open-dialog

# Specify render scale for screenshots
cargo run -- --screenshot output.png --scale 2.0
```

## Project Structure

```
src/
├── main.rs                     # App struct, event loop, draw pipeline, CLI (~2,850 lines)
├── git.rs                      # Git operations (git2 + CLI wrappers, ~1,330 lines)
├── config.rs                   # Settings persistence (serde_json → ~/.config/whisper-git/)
├── input/
│   ├── mod.rs                  # InputState, InputEvent types
│   ├── keyboard.rs             # Key definitions and modifier tracking
│   └── mouse.rs                # Mouse button and position state
├── renderer/
│   ├── mod.rs                  # Module exports
│   ├── context.rs              # VulkanContext: device, queue, allocators
│   ├── surface.rs              # Swapchain and framebuffer management
│   ├── screenshot.rs           # Screenshot capture to PNG
│   └── offscreen.rs            # Offscreen render target for controlled-size captures
├── ui/
│   ├── mod.rs                  # Module exports, Color, Rect
│   ├── text.rs                 # Font atlas text renderer (TextVertex, TextRenderer)
│   ├── spline.rs               # Bezier curve and line renderer (SplineVertex, SplineRenderer)
│   ├── avatar.rs               # Gravatar download, disk cache, GPU atlas (AvatarCache, AvatarRenderer)
│   ├── widget.rs               # Widget trait, WidgetState, theme constants
│   ├── layout/
│   │   ├── mod.rs              # Rect primitives with rectangle math
│   │   ├── screen.rs           # ScreenLayout (header, shortcut bar, sidebar, graph, staging, secondary)
│   │   └── flex.rs             # Flex layout system
│   └── widgets/
│       ├── mod.rs              # Widget re-exports
│       ├── button.rs           # Clickable button with hover/press states
│       ├── context_menu.rs     # Right-click popup overlay with keyboard nav
│       ├── file_list.rs        # File list with staging toggle and status dots
│       ├── header_bar.rs       # Top bar: branch pill, Fetch/Pull/Push/Commit buttons, drop shadow
│       ├── label.rs            # Text label
│       ├── panel.rs            # Container widget
│       ├── repo_dialog.rs      # Modal dialog for opening repositories
│       ├── scrollbar.rs        # Proportional scrollbar with drag support
│       ├── search_bar.rs       # Search/filter input bar
│       ├── settings_dialog.rs  # Settings modal (avatars, scroll speed, row size)
│       ├── shortcut_bar.rs     # Context-sensitive keyboard shortcut hints
│       ├── tab_bar.rs          # Tab bar for multi-repository support
│       ├── text_area.rs        # Multi-line text editor
│       ├── text_input.rs       # Single-line text input with cursor
│       ├── branch_name_dialog.rs # Branch/tag creation name input modal
│       ├── confirm_dialog.rs  # Confirmation dialog for destructive operations
│       └── toast.rs            # Toast notification manager
└── views/
    ├── mod.rs                  # View re-exports
    ├── branch_sidebar.rs       # Sidebar: branches/tags/submodules/worktrees/stashes (~1,415 lines)
    ├── commit_detail.rs        # Full commit metadata and file list
    ├── commit_graph.rs         # Commit graph with spline branches and search (~1,504 lines)
    ├── diff_view.rs            # Color-coded diff with word-level highlights
    ├── secondary_repos.rs      # (Legacy) Submodule/worktree cards - data moved to sidebar
    └── staging_well.rs         # Staging panel with commit message editor + amend mode
```

## Architecture

### Screen Layout

The screen is divided into drag-resizable regions by `ScreenLayout`:

```
┌──────────────────────────────────────────────────────────┐
│ [Tab Bar]  (only visible when multiple tabs are open)    │
├──────────────────────────────────────────────────────────┤
│ [Header Bar]  branch pill | Fetch Pull Push | Commit  ?= │
├──────────────────────────────────────────────────────────┤
│ [Shortcut Bar]  context-sensitive key pills (toggleable) │
├─────────┬──────────────────────┬─────────────────────────┤
│ Branch  │                      │   Staging Well          │
│ Sidebar │   Commit Graph       │   (subject, body,       │
│ LOCAL   │   (drag-resizable)   │    staged/unstaged)     │
│ REMOTE  │                      ├ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ┤
│ TAGS    │                      │   Diff / Detail View    │
│ SUBMODS │                      │   (drag-resizable       │
│ WORKTRS │                      │    vertical split)      │
│ STASHES │                      │                         │
├─────────┴──────────────────────┴─────────────────────────┤
│ [Toast Notifications]  (bottom-center overlay)           │
└──────────────────────────────────────────────────────────┘
```

Panel dividers are drag-resizable with configurable ratios (sidebar 5-30%, graph 30-80%, staging 15-85%).

### Render Pipeline

Each frame generates vertex data through an immediate-mode pattern with three z-ordered layers:

1. Views and widgets produce `WidgetOutput` containing `text_vertices`, `spline_vertices`, and `avatar_vertices`
2. Outputs are collected into three layers for correct z-ordering:
   - **Graph layer**: panel backgrounds + commit graph (splines, nodes, pills, text)
   - **Chrome layer**: header bar, shortcut bar, sidebar, staging well, diff viewer
   - **Overlay layer**: context menus, toasts, confirmation dialogs, modal dialogs
3. Each layer renders three passes in order:
   - **Spline pass**: backgrounds, borders, shapes, graph lines, rounded rects
   - **Avatar pass**: Gravatar images from the avatar atlas
   - **Text pass**: all text content from the font atlas

### Git Integration

- Repository access via `git2` (libgit2 Rust bindings)
- Remote operations (fetch, pull, push) shell out to the `git` CLI in background threads using `std::thread` + `mpsc::channel`
- Hunk staging/unstaging uses `git apply --cached` via CLI
- File discard uses `git2::Repository::checkout_head()` with force

### Bare Repo + Worktree Support

Whisper Git handles the common bare-repo-with-worktrees layout:

```
whisper-git/
├── .bare/          # Bare git repository
├── .git            # File pointing to .bare/
├── main/           # Main branch worktree
└── feature-x/      # Feature branch worktree
```

The `GitRepo::open()` function includes fallback logic: if HEAD points to a stale branch (common in bare repos), it walks local branch tips to find valid commits.

## Development

### Worktree Workflow

This project uses git worktrees for parallel development:

```bash
cd whisper-git
git worktree add feature-name -b feature-name
```

### Screenshot Testing

The `--screenshot` flag renders the UI and captures it to a PNG file without requiring interactive use. This is useful for CI pipelines and for providing visual context to LLM-assisted development tools.

```bash
# Basic screenshot
cargo run -- --screenshot output.png --size 1920x1080

# Screenshot with injected UI state
cargo run -- --screenshot output.png --size 1920x1080 --screenshot-state commit-detail
```

Available screenshot states: `open-dialog`, `search`, `context-menu`, `commit-detail`.

## Documentation

- [docs/render_engine.md](docs/render_engine.md) -- Rendering architecture, Vulkan pipeline, vertex generation
- [docs/ux-design-2026-02.md](docs/ux-design-2026-02.md) -- UI/UX specification and design rationale
- [docs/user_needs.md](docs/user_needs.md) -- Core user needs and feature priorities
- [docs/design_feedback_feb2026.md](docs/design_feedback_feb2026.md) -- Design review and strategic recommendations

## License

TBD
