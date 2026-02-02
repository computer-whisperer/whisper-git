use crate::git::CommitInfo;
use crate::ui::{Color, Rect, TextRenderer, TextVertex};

/// View for displaying a list of commits
pub struct CommitListView {
    title_color: Color,
    commit_color: Color,
    max_commits: usize,
}

impl Default for CommitListView {
    fn default() -> Self {
        Self {
            title_color: Color::rgba(0.9, 0.9, 0.95, 1.0),
            commit_color: Color::rgba(0.7, 0.75, 0.8, 1.0),
            max_commits: 15,
        }
    }
}

impl CommitListView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_commits(mut self, max: usize) -> Self {
        self.max_commits = max;
        self
    }

    /// Generate vertices for rendering the commit list
    pub fn layout(
        &self,
        text_renderer: &TextRenderer,
        commits: &[CommitInfo],
        bounds: Rect,
    ) -> Vec<TextVertex> {
        let mut vertices = Vec::new();
        let line_height = text_renderer.line_height();
        let mut y = bounds.y + 20.0; // Padding from top

        // Title
        vertices.extend(text_renderer.layout_text(
            "Recent Commits",
            bounds.x + 20.0,
            y,
            self.title_color.to_array(),
        ));
        y += line_height * 1.5;

        // Commits
        for commit in commits.iter().take(self.max_commits) {
            let text = format!("{} {}", commit.short_id, commit.summary);
            // Truncate long lines based on available width
            let max_chars = ((bounds.width - 40.0) / 10.0) as usize; // Rough estimate
            let text = if text.len() > max_chars && max_chars > 3 {
                format!("{}...", &text[..max_chars - 3])
            } else {
                text
            };

            vertices.extend(text_renderer.layout_text(
                &text,
                bounds.x + 20.0,
                y,
                self.commit_color.to_array(),
            ));
            y += line_height;

            // Stop if we're outside bounds
            if y > bounds.bottom() {
                break;
            }
        }

        vertices
    }
}
