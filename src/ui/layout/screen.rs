//! Screen layout builder - creates the main application layout

use super::Rect;

/// The computed layout regions for the main screen
#[derive(Clone, Debug)]
pub struct ScreenLayout {
    /// Header bar region (4% height)
    pub header: Rect,
    /// Primary commit graph region (55% width, 96% height)
    pub graph: Rect,
    /// Staging well region (45% width, 45% of main height)
    pub staging: Rect,
    /// Secondary repos region (45% width, 51% of main height)
    pub secondary_repos: Rect,
}

impl ScreenLayout {
    /// Create the screen layout from the given window bounds
    ///
    /// Layout structure:
    /// ```text
    /// +----------------------------------------------------------+
    /// |                     HEADER (4%)                          |
    /// +----------------------------------------------------------+
    /// |                          |                               |
    /// |    GRAPH (55% width)     |   STAGING (45% x 45%)         |
    /// |                          |                               |
    /// |                          +-------------------------------+
    /// |                          |                               |
    /// |                          |   SECONDARY (45% x 51%)       |
    /// |                          |                               |
    /// +----------------------------------------------------------+
    /// ```
    pub fn compute(bounds: Rect) -> Self {
        // Split into header and main area
        let header_height = bounds.height * 0.04;
        let (header, main) = bounds.take_top(header_height.max(32.0)); // Min 32px header

        // Split main area into left (graph) and right (staging + secondary)
        let (graph, right_panel) = main.split_horizontal(0.55);

        // Split right panel into staging (top 45%) and secondary (bottom 51%)
        // The remaining 4% is implicitly used for spacing/gaps
        let staging_height = right_panel.height * 0.45;
        let (staging, remaining) = right_panel.take_top(staging_height);

        // Secondary repos takes the rest
        let secondary_repos = remaining;

        Self {
            header,
            graph,
            staging,
            secondary_repos,
        }
    }

    /// Create a layout with a gap between sections
    pub fn compute_with_gap(bounds: Rect, gap: f32) -> Self {
        let base = Self::compute(bounds);

        // Apply gap padding
        Self {
            header: base.header.pad(gap, gap, gap, 0.0),
            graph: base.graph.pad(gap, gap, gap / 2.0, gap),
            staging: base.staging.pad(gap / 2.0, gap, gap, gap / 2.0),
            secondary_repos: base.secondary_repos.pad(gap / 2.0, gap / 2.0, gap, gap),
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

        // Graph should take 55% width
        assert!((layout.graph.width - 704.0).abs() < 1.0); // 1280 * 0.55 = 704

        // Staging should take 45% width
        assert!((layout.staging.width - 576.0).abs() < 1.0); // 1280 * 0.45 = 576
    }
}
