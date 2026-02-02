# Whisper Git

A GPU-accelerated Git client built with Vulkano. Designed for power users who want visualization of advanced Git features like worktrees and submodules.

## Status

**Early Development** - Core infrastructure and UI widget system implemented.

### What Works
- **Rendering Engine**
  - Vulkano pipeline with custom text rendering (no egui dependency)
  - Font atlas generation from system fonts (DejaVu Sans Mono)
  - Spline-based line rendering with consistent thickness
  - Screenshot capture for CI/testing (`--screenshot`)
  - Offscreen rendering at arbitrary resolutions (`--size WxH`)

- **Commit Graph**
  - Branch topology visualization with GPU-accelerated splines
  - Lane assignment algorithm for parallel branches
  - Bezier curves for merge/fork connections
  - Colored lanes for visual distinction

- **Input System**
  - Unified keyboard and mouse event handling
  - Modifier key tracking (Shift, Ctrl, Alt, Meta)

- **Widget System**
  - Core `Widget` trait with event handling, layout, and rendering
  - Widget state tracking (hovered, focused, pressed, enabled)
  - Dark theme with consistent color palette

- **Layout System**
  - `Rect` primitive with rectangle math (split, inset, pad, take_*)
  - `ScreenLayout` implementing the UX spec (55% graph, 45% staging)
  - Flex layout foundation

- **UI Components**
  - Header bar with repository info and action buttons
  - Text input (single-line) and text area (multi-line)
  - File list with staging toggle
  - Button, label, and panel primitives

- **Views**
  - `CommitGraphView` - commit history visualization
  - `StagingWell` - commit message editor with staged/unstaged file lists

- **Git Integration**
  - Repository loading via git2
  - Commit info, branch refs, working directory status

### Planned
- LLM-generated commit message suggestions
- Secondary repos panel (submodules/worktrees)
- Commit detail view
- Context menus and command palette
- Worktree management
- Interactive rebase interface

## Building

```bash
cd main
cargo build --release
```

Requires:
- Rust 2024 edition
- Vulkan-capable GPU
- System font: `/usr/share/fonts/TTF/DejaVuSansMono.ttf`

## Usage

```bash
# Open a repository
cargo run -- /path/to/repo

# Open current directory
cargo run

# Screenshot mode (for CI/LLM agents)
cargo run -- --repo /path/to/repo --screenshot output.png

# Screenshot at specific resolution (bypasses window manager)
cargo run -- --screenshot output.png --size 1920x1080
cargo run -- --screenshot output.png --size 3840x2160  # 4K
```

## Project Structure

```
src/
├── main.rs                 # Entry point, CLI, event loop
├── git.rs                  # Git operations (git2 wrapper)
├── input/                  # Unified input handling
│   ├── mod.rs              # InputState, InputEvent types
│   ├── keyboard.rs         # Key definitions and state
│   └── mouse.rs            # Mouse state tracking
├── renderer/
│   ├── context.rs          # VulkanContext: device, queue, allocators
│   ├── surface.rs          # Swapchain and framebuffer management
│   ├── screenshot.rs       # Screenshot capture to PNG
│   └── offscreen.rs        # Offscreen render target for controlled-size captures
├── ui/
│   ├── text.rs             # Font atlas text renderer
│   ├── spline.rs           # Bezier curve and line renderer
│   ├── widget.rs           # Widget trait and WidgetState
│   ├── layout/
│   │   ├── mod.rs          # Rect, Color primitives
│   │   ├── screen.rs       # ScreenLayout (header, graph, staging)
│   │   └── flex.rs         # Flex layout system
│   └── widgets/
│       ├── button.rs       # Clickable button
│       ├── header_bar.rs   # Top bar with repo info and actions
│       ├── label.rs        # Text label
│       ├── panel.rs        # Container widget
│       ├── text_input.rs   # Single-line text input
│       ├── text_area.rs    # Multi-line text editor
│       └── file_list.rs    # File list with staging toggle
└── views/
    ├── commit_graph.rs     # Commit graph with spline branches
    ├── commit_list.rs      # Simple commit list (legacy)
    └── staging_well.rs     # Staging panel with commit message
```

## Documentation

- [docs/render_engine.md](docs/render_engine.md) - Rendering architecture, Vulkan pipeline, vertex generation
- [docs/ux-design-2026-02.md](docs/ux-design-2026-02.md) - UI/UX specification and design rationale

## Development

This project uses git worktrees for parallel development:

```
whisper-git/
├── .bare/          # Bare git repository
├── .git            # Points to .bare
├── main/           # Main branch worktree (you are here)
└── feature-x/      # Feature branches as sibling directories
```

To add a new worktree:
```bash
cd whisper-git
git worktree add feature-name -b feature-name
```

## License

TBD
