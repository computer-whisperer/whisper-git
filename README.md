# Whisper

A GPU-accelerated Git client built with Vulkano. Designed for power users who want visualization of advanced Git features like worktrees and submodules.

## Status

**Early Development** - Basic commit list rendering works.

### What Works
- Vulkano rendering pipeline with custom text rendering (no egui)
- Font atlas generation from system fonts (DejaVu Sans Mono)
- Git repository loading via git2
- Recent commit display
- Screenshot capture for CI/testing (`--screenshot`)

### In Progress
- Commit graph visualization
- UI widget system

### Planned
- Worktree management and visualization
- Submodule visualization
- Interactive commit browsing
- Branch management

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
```

## Project Structure

```
src/
├── main.rs              # Entry point, CLI, event loop
├── git.rs               # Git operations (git2 wrapper)
├── renderer/
│   ├── context.rs       # VulkanContext: device, queue, allocators
│   ├── surface.rs       # Swapchain and framebuffer management
│   └── screenshot.rs    # Screenshot capture to PNG
├── ui/
│   ├── text.rs          # Font atlas text renderer
│   ├── layout.rs        # Rect, Color primitives
│   └── widgets/         # UI components (planned)
└── views/
    └── commit_list.rs   # Commit list view
```

## Architecture

See [docs/render_engine.md](docs/render_engine.md) for rendering architecture details.

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
