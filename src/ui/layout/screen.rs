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
    /// Primary commit graph region
    pub graph: Rect,
    /// Staging well region (left portion of right area)
    pub staging: Rect,
    /// Preview panel region for diff/detail views (right portion of right area)
    pub preview: Rect,
}

impl ScreenLayout {
    /// Create the screen layout from the given window bounds
    ///
    /// Layout structure:
    /// ```text
    /// +----------------------------------------------------------+
    /// |                     HEADER (4%)                          |
    /// +------+---------------------------------------------------+
    /// |      |                |            |                     |
    /// | SIDE |   GRAPH        |  STAGING   |   PREVIEW           |
    /// | BAR  |                |            |                     |
    /// | 180  |                |            |                     |
    /// | px   |                |            |                     |
    /// |      |                |            |                     |
    /// |      |                |            |                     |
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
        let shortcut_bar_height = 26.0 * scale;
        let (shortcut_bar, main) = after_header.take_top(shortcut_bar_height);

        // Split main area into sidebar and content
        let sidebar_width = (180.0 * scale).min(main.width * 0.15);
        let (sidebar, content) = main.take_left(sidebar_width);

        // Split content area into graph and right area
        let (graph, right_area) = content.split_horizontal(0.55);

        // Split right area into staging and preview (both full height)
        let (staging, preview) = right_area.split_horizontal(0.40);

        Self {
            header,
            shortcut_bar,
            sidebar,
            graph,
            staging,
            preview,
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
    /// - `staging_ratio`: fraction of right area width for staging (default 0.40, clamped 0.20..0.70)
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
        Self::compute_with_ratios_and_shortcut(bounds, gap, scale, sidebar_ratio, graph_ratio, staging_ratio, true)
    }

    /// Like `compute_with_ratios` but allows hiding the shortcut bar.
    pub fn compute_with_ratios_and_shortcut(
        bounds: Rect,
        gap: f32,
        scale: f32,
        sidebar_ratio: Option<f32>,
        graph_ratio: Option<f32>,
        staging_ratio: Option<f32>,
        shortcut_bar_visible: bool,
    ) -> Self {
        // Split into header and main area
        let header_height = bounds.height * 0.04;
        let (header, after_header) = bounds.take_top(header_height.max(32.0 * scale));

        // Shortcut bar: thin strip below header (zero height when hidden)
        let shortcut_bar_height = if shortcut_bar_visible { 26.0 * scale } else { 0.0 };
        let (shortcut_bar, main) = after_header.take_top(shortcut_bar_height);

        // Sidebar width: use ratio if provided, otherwise default ~180px
        let sidebar_width = if let Some(ratio) = sidebar_ratio {
            let clamped = ratio.clamp(0.05, 0.30);
            main.width * clamped
        } else {
            (180.0 * scale).min(main.width * 0.15)
        };
        let (sidebar, content) = main.take_left(sidebar_width);

        // Graph / right area split
        let graph_frac = graph_ratio.unwrap_or(0.55).clamp(0.30, 0.80);
        let (graph, right_area) = content.split_horizontal(graph_frac);

        // Staging / preview horizontal split (both full height)
        let staging_frac = staging_ratio.unwrap_or(0.40).clamp(0.20, 0.70);
        let (staging, preview) = right_area.split_horizontal(staging_frac);

        // Apply gap padding
        Self {
            header: header.pad(gap, gap, gap, 0.0),
            shortcut_bar, // no padding - full width subtle bar
            sidebar: sidebar.pad(gap, gap, gap / 2.0, gap),
            graph: graph.pad(gap / 2.0, gap, gap / 2.0, gap),
            staging: staging.pad(gap / 2.0, gap, gap / 2.0, gap),
            preview: preview.pad(gap / 2.0, gap, gap, gap),
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

        // Right area is 45% of remaining = 495
        // Staging takes 40% of right area = 198
        assert!((layout.staging.width - 198.0).abs() < 1.0);
        // Preview takes 60% of right area = 297
        assert!((layout.preview.width - 297.0).abs() < 1.0);

        // Both staging and preview are full height
        assert!((layout.staging.height - layout.graph.height).abs() < 1.0);
        assert!((layout.preview.height - layout.graph.height).abs() < 1.0);
    }
}
