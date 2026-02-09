//! Screen layout builder - creates the main application layout

use super::Rect;

/// The computed layout regions for the main screen
#[derive(Clone, Debug)]
pub struct ScreenLayout {
    /// Header bar region (4% height)
    pub header: Rect,
    /// Keyboard shortcut status bar (thin bar below header)
    pub shortcut_bar: Rect,
    /// Branch sidebar region (180px wide, left side)
    pub sidebar: Rect,
    /// Primary commit graph region (55% of remaining width, 96% height)
    pub graph: Rect,
    /// Staging well region (45% of remaining width, 45% of main height)
    pub staging: Rect,
    /// Right panel region for diff/detail views (45% of remaining width, bottom portion)
    pub right_panel: Rect,
}

impl ScreenLayout {
    /// Create the screen layout from the given window bounds
    ///
    /// Layout structure:
    /// ```text
    /// +----------------------------------------------------------+
    /// |                     HEADER (4%)                          |
    /// +------+---------------------------------------------------+
    /// |      |                       |                           |
    /// | SIDE |   GRAPH (55% rem)     |   STAGING (45% x 45%)    |
    /// | BAR  |                       |                           |
    /// | 180  |                       +---------------------------+
    /// | px   |                       |                           |
    /// |      |                       |   RIGHT PANEL (45% x 55%)|
    /// |      |                       |                           |
    /// +------+---------------------------------------------------+
    /// ```
    #[allow(dead_code)]
    pub fn compute(bounds: Rect) -> Self {
        Self::compute_scaled(bounds, 1.0)
    }

    /// Create the screen layout, with pixel constants scaled for HiDPI
    pub fn compute_scaled(bounds: Rect, scale: f32) -> Self {
        // Split into header and main area
        let header_height = bounds.height * 0.04;
        let (header, after_header) = bounds.take_top(header_height.max(32.0 * scale));

        // Shortcut bar: thin strip below header
        let shortcut_bar_height = 20.0 * scale;
        let (shortcut_bar, main) = after_header.take_top(shortcut_bar_height);

        // Split main area into sidebar and content
        let sidebar_width = (180.0 * scale).min(main.width * 0.15);
        let (sidebar, content) = main.take_left(sidebar_width);

        // Split content area into left (graph) and right (staging + secondary)
        let (graph, right_panel) = content.split_horizontal(0.55);

        // Split right panel into staging (top 45%) and secondary (bottom 51%)
        // The remaining 4% is implicitly used for spacing/gaps
        let staging_height = right_panel.height * 0.45;
        let (staging, remaining) = right_panel.take_top(staging_height);

        // Secondary repos takes the rest
        let right_panel = remaining;

        Self {
            header,
            shortcut_bar,
            sidebar,
            graph,
            staging,
            right_panel,
        }
    }

    /// Create a layout with a gap between sections (convenience wrapper using default ratios)
    #[allow(dead_code)]
    pub fn compute_with_gap(bounds: Rect, gap: f32, scale: f32) -> Self {
        Self::compute_with_ratios(bounds, gap, scale, None, None, None)
    }

    /// Create the screen layout with custom panel ratios and gap padding.
    ///
    /// - `sidebar_ratio`: fraction of total width for sidebar (default ~0.14, clamped 0.05..0.30)
    /// - `graph_ratio`: fraction of content width (after sidebar) for graph (default 0.55, clamped 0.30..0.80)
    /// - `staging_ratio`: fraction of right panel height for staging (default 0.45, clamped 0.15..0.85)
    ///
    /// Pass `None` for any ratio to use the default value.
    pub fn compute_with_ratios(
        bounds: Rect,
        gap: f32,
        scale: f32,
        sidebar_ratio: Option<f32>,
        graph_ratio: Option<f32>,
        staging_ratio: Option<f32>,
    ) -> Self {
        // Split into header and main area
        let header_height = bounds.height * 0.04;
        let (header, after_header) = bounds.take_top(header_height.max(32.0 * scale));

        // Shortcut bar: thin strip below header
        let shortcut_bar_height = 20.0 * scale;
        let (shortcut_bar, main) = after_header.take_top(shortcut_bar_height);

        // Sidebar width: use ratio if provided, otherwise default ~180px
        let sidebar_width = if let Some(ratio) = sidebar_ratio {
            let clamped = ratio.clamp(0.05, 0.30);
            main.width * clamped
        } else {
            (180.0 * scale).min(main.width * 0.15)
        };
        let (sidebar, content) = main.take_left(sidebar_width);

        // Graph / right panel split
        let graph_frac = graph_ratio.unwrap_or(0.55).clamp(0.30, 0.80);
        let (graph, right_panel) = content.split_horizontal(graph_frac);

        // Staging / right_panel vertical split
        let staging_frac = staging_ratio.unwrap_or(0.45).clamp(0.15, 0.85);
        let staging_height = right_panel.height * staging_frac;
        let (staging, remaining) = right_panel.take_top(staging_height);
        let right_panel = remaining;

        // Apply gap padding
        Self {
            header: header.pad(gap, gap, gap, 0.0),
            shortcut_bar, // no padding - full width subtle bar
            sidebar: sidebar.pad(gap, gap, gap / 2.0, gap),
            graph: graph.pad(gap / 2.0, gap, gap / 2.0, gap),
            staging: staging.pad(gap / 2.0, gap, gap, gap / 2.0),
            right_panel: right_panel.pad(gap / 2.0, gap / 2.0, gap, gap),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_screen_layout() {
        let bounds = Rect::from_size(1280.0, 720.0);
        let layout = ScreenLayout::compute(bounds);

        // Header should be at top
        assert_eq!(layout.header.y, 0.0);
        assert!(layout.header.height >= 28.0); // 4% of 720 = 28.8

        // Sidebar should be 180px wide
        assert!((layout.sidebar.width - 180.0).abs() < 1.0);

        // Remaining width after sidebar: 1280 - 180 = 1100
        // Graph should take 55% of remaining
        assert!((layout.graph.width - 605.0).abs() < 1.0); // 1100 * 0.55 = 605

        // Staging should take 45% of remaining
        assert!((layout.staging.width - 495.0).abs() < 1.0); // 1100 * 0.45 = 495
    }
}
