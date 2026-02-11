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
    /// Right panel region (staging + preview merged)
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
    /// |      |                |                                  |
    /// | SIDE |   GRAPH        |   RIGHT PANEL                   |
    /// | BAR  |                |   (staging + preview)            |
    /// | 180  |                |                                  |
    /// | px   |                |                                  |
    /// |      |                |                                  |
    /// |      |                |                                  |
    /// +------+---------------------------------------------------+
    /// ```
    #[cfg(test)]
    pub fn compute(bounds: Rect) -> Self {
        Self::compute_with_ratios_and_shortcut(bounds, 0.0, 1.0, None, None, true)
    }

    /// Create the screen layout with custom panel ratios, gap padding, and shortcut bar toggle.
    pub fn compute_with_ratios_and_shortcut(
        bounds: Rect,
        gap: f32,
        scale: f32,
        sidebar_ratio: Option<f32>,
        graph_ratio: Option<f32>,
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

        // Graph / right panel split
        let graph_frac = graph_ratio.unwrap_or(0.55).clamp(0.30, 0.80);
        let (graph, right_panel) = content.split_horizontal(graph_frac);

        // Apply gap padding
        Self {
            header: header.pad(gap, gap, gap, 0.0),
            shortcut_bar, // no padding - full width subtle bar
            sidebar: sidebar.pad(gap, gap, gap / 2.0, gap),
            graph: graph.pad(gap / 2.0, gap, gap / 2.0, gap),
            right_panel: right_panel.pad(gap / 2.0, gap, gap, gap),
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

        // Right panel is 45% of remaining = 495
        assert!((layout.right_panel.width - 495.0).abs() < 1.0);

        // Right panel is full height
        assert!((layout.right_panel.height - layout.graph.height).abs() < 1.0);
    }
}
