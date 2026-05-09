//! Unified-diff viewer widget — Github-shaped: colored gutter strip
//! per line, subtle row-level wash, optional word-level intra-line
//! highlights, and per-hunk action buttons.
//!
//! Pure data-in: the widget takes a [`DiffData`] value and renders it.
//! Whisper-git's `diff_view::diff_view(tab)` is the thin adapter that
//! resolves the right hunks (working-tree vs commit) and builds the
//! input. Once the API settles this module is a candidate for moving
//! upstream into aetna's catalog.

use aetna_core::{El, prelude::*};

/// One line in a diff. The `kind` drives row + gutter color; the
/// optional `highlights` are byte ranges within `content` that paint
/// with a brighter background (intra-line / word-level diff).
#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub content: String,
    pub old_lineno: Option<u32>,
    pub new_lineno: Option<u32>,
    pub highlights: Vec<(usize, usize)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffLineKind {
    /// Unchanged context (shown both pre and post).
    Context,
    /// Added in this revision (`+`).
    Addition,
    /// Removed in this revision (`-`).
    Deletion,
}

/// One hunk: the `@@ -a,b +c,d @@` header and the run of lines that
/// follow. `action` is an optional Stage / Unstage button rendered in
/// the hunk header; the app pre-routes the click key.
#[derive(Clone, Debug)]
pub struct DiffHunk {
    /// The full `@@` header line as emitted by libgit2 (e.g.
    /// `@@ -10,5 +10,7 @@ fn main()`).
    pub header: String,
    pub lines: Vec<DiffLine>,
    pub action: Option<DiffHunkAction>,
}

#[derive(Clone, Debug)]
pub struct DiffHunkAction {
    pub label: String,
    pub key: String,
    pub tooltip: Option<String>,
}

/// Top-level diff payload. `title` is the path; `badge` is the
/// secondary tag shown in the header (e.g. `staged`, `unstaged`, or a
/// commit short SHA).
#[derive(Clone, Debug)]
pub struct DiffData {
    pub title: String,
    pub badge: Option<String>,
    pub hunks: Vec<DiffHunk>,
}

impl DiffData {
    /// Sum the additions and deletions across all hunks.
    pub fn stats(&self) -> (usize, usize) {
        let mut adds = 0usize;
        let mut dels = 0usize;
        for h in &self.hunks {
            for l in &h.lines {
                match l.kind {
                    DiffLineKind::Addition => adds += 1,
                    DiffLineKind::Deletion => dels += 1,
                    DiffLineKind::Context => {}
                }
            }
        }
        (adds, dels)
    }
}

const LINENO_COL_WIDTH: f32 = 44.0;
const ROW_BG_ALPHA: u8 = 48;
const HIGHLIGHT_BG_ALPHA: u8 = 130;

#[track_caller]
pub fn diff(data: &DiffData) -> El {
    let (adds, dels) = data.stats();

    let mut header_children: Vec<El> = vec![text(data.title.clone()).label(), spacer()];
    if !data.hunks.is_empty() {
        header_children.push(
            text(format!("+{adds}"))
                .mono()
                .caption()
                .text_color(tokens::SUCCESS),
        );
        header_children.push(
            text(format!("-{dels}"))
                .mono()
                .caption()
                .text_color(tokens::DESTRUCTIVE),
        );
    }
    if let Some(b) = data.badge.as_ref() {
        header_children.push(badge(b.clone()).muted());
    }
    let header_row = row(header_children)
        .gap(tokens::SPACE_2)
        .align(Align::Center);

    let body: El = if data.hunks.is_empty() {
        column([text("(no changes)").caption().muted()]).padding(tokens::SPACE_4)
    } else {
        let blocks: Vec<El> = data.hunks.iter().map(hunk_block).collect();
        column(blocks).gap(tokens::SPACE_3).padding(tokens::SPACE_3)
    };

    card([
        card_header([header_row])
            .padding(Sides::xy(tokens::SPACE_4, tokens::SPACE_2))
            .fill(tokens::MUTED),
        card_content([scroll([body])
            .key("diff:scroll")
            .height(Size::Fill(1.0))])
        .padding(0.0)
        .height(Size::Fill(1.0)),
    ])
    .height(Size::Fill(1.0))
    .width(Size::Fill(1.0))
}

fn hunk_block(hunk: &DiffHunk) -> El {
    let mut header_children: Vec<El> = Vec::with_capacity(3);
    let (range, context) = split_hunk_header(&hunk.header);
    header_children.push(text(range).code().text_color(tokens::INFO));
    if let Some(ctx) = context {
        header_children.push(text(ctx).code().muted());
    }
    header_children.push(spacer());
    if let Some(action) = hunk.action.as_ref() {
        let mut btn = button(action.label.clone()).key(action.key.clone()).ghost();
        if let Some(tip) = action.tooltip.as_ref() {
            btn = btn.tooltip(tip.clone());
        }
        header_children.push(btn);
    }
    let header_row = row(header_children)
        .gap(tokens::SPACE_2)
        .align(Align::Center);

    let lines: Vec<El> = hunk.lines.iter().map(line_row).collect();

    card([
        card_header([header_row])
            .padding(Sides::xy(tokens::SPACE_2, tokens::SPACE_1))
            .fill(tokens::MUTED),
        card_content(lines).padding(0.0),
    ])
}

/// Parse `@@ -a,b +c,d @@ <context>` into the range chunk
/// (`@@ -a,b +c,d @@`) and the optional trailing context (e.g.
/// `fn main()`). The trailing context is what most diff viewers
/// surface as a quiet hint about which function/section the hunk
/// lives in.
fn split_hunk_header(header: &str) -> (String, Option<String>) {
    let trimmed = header.trim();
    // The header is `@@ ... @@ <maybe context>`. Find the second `@@`.
    if let Some(first) = trimmed.find("@@") {
        let after_first = &trimmed[first + 2..];
        if let Some(second) = after_first.find("@@") {
            let range_end = first + 2 + second + 2;
            let range = trimmed[..range_end].trim().to_string();
            let context = trimmed[range_end..].trim();
            if context.is_empty() {
                return (range, None);
            }
            return (range, Some(context.to_string()));
        }
    }
    (trimmed.to_string(), None)
}

fn line_row(line: &DiffLine) -> El {
    let row_bg = match line.kind {
        DiffLineKind::Addition => Some(tokens::SUCCESS.with_alpha(ROW_BG_ALPHA)),
        DiffLineKind::Deletion => Some(tokens::DESTRUCTIVE.with_alpha(ROW_BG_ALPHA)),
        DiffLineKind::Context => None,
    };

    let old_no = line
        .old_lineno
        .map(|n| n.to_string())
        .unwrap_or_default();
    let new_no = line
        .new_lineno
        .map(|n| n.to_string())
        .unwrap_or_default();

    let row_el = row([
        text(format!("{old_no:>4}"))
            .mono()
            .caption()
            .muted()
            .nowrap_text()
            .width(Size::Fixed(LINENO_COL_WIDTH))
            .padding(Sides::xy(tokens::SPACE_2, 0.0)),
        text(format!("{new_no:>4}"))
            .mono()
            .caption()
            .muted()
            .nowrap_text()
            .width(Size::Fixed(LINENO_COL_WIDTH))
            .padding(Sides::xy(tokens::SPACE_2, 0.0)),
        line_content(line),
    ])
    .align(Align::Center);

    if let Some(bg) = row_bg {
        row_el.fill(bg)
    } else {
        row_el
    }
}

/// Render a line's content as a single shaped row. When the line
/// carries `highlights`, the row is split into per-range mono text
/// runs so the brighter highlight background paints exactly under
/// the changed bytes (mono ensures char-aligned widths). Text stays
/// FOREGROUND across the board — the row bg + word highlight bg
/// carry the color signal, and Github-style colored text on tinted
/// bg hurts readability for context-rich diffs.
fn line_content(line: &DiffLine) -> El {
    let highlight_bg = match line.kind {
        DiffLineKind::Addition => Some(tokens::SUCCESS.with_alpha(HIGHLIGHT_BG_ALPHA)),
        DiffLineKind::Deletion => Some(tokens::DESTRUCTIVE.with_alpha(HIGHLIGHT_BG_ALPHA)),
        DiffLineKind::Context => None,
    };

    let trimmed = line.content.trim_end_matches('\n');
    let runs = split_into_runs(trimmed, &line.highlights);
    if runs.len() == 1 || highlight_bg.is_none() {
        // No highlights (or context line) — single span, simpler tree.
        return text(trimmed.to_string())
            .mono()
            .nowrap_text()
            .padding(Sides::xy(tokens::SPACE_2, 0.0))
            .width(Size::Fill(1.0));
    }
    let bg = highlight_bg.expect("checked above");
    let children: Vec<El> = runs
        .into_iter()
        .map(|(span, hot)| {
            let t = text(span).mono().nowrap_text();
            if hot { t.fill(bg) } else { t }
        })
        .collect();
    row(children)
        .padding(Sides::xy(tokens::SPACE_2, 0.0))
        .width(Size::Fill(1.0))
}

/// Split `content` into `(span, is_highlighted)` segments at the
/// given byte ranges. Skips ranges that aren't on UTF-8 char
/// boundaries (defensive — libgit2 returns byte offsets that should
/// be valid, but a malformed diff shouldn't crash the renderer).
fn split_into_runs(content: &str, highlights: &[(usize, usize)]) -> Vec<(String, bool)> {
    if highlights.is_empty() {
        return vec![(content.to_string(), false)];
    }
    let mut sorted: Vec<(usize, usize)> = highlights
        .iter()
        .filter(|(s, e)| {
            *s < *e
                && *e <= content.len()
                && content.is_char_boundary(*s)
                && content.is_char_boundary(*e)
        })
        .copied()
        .collect();
    sorted.sort_by_key(|(s, _)| *s);

    let mut out: Vec<(String, bool)> = Vec::with_capacity(sorted.len() * 2 + 1);
    let mut cursor = 0usize;
    for (start, end) in sorted {
        if start < cursor {
            // Overlapping range — skip rather than panic.
            continue;
        }
        if start > cursor {
            out.push((content[cursor..start].to_string(), false));
        }
        out.push((content[start..end].to_string(), true));
        cursor = end;
    }
    if cursor < content.len() {
        out.push((content[cursor..].to_string(), false));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_sums_additions_and_deletions_across_hunks() {
        let line = |kind| DiffLine {
            kind,
            content: String::new(),
            old_lineno: None,
            new_lineno: None,
            highlights: Vec::new(),
        };
        let data = DiffData {
            title: "f.rs".into(),
            badge: None,
            hunks: vec![
                DiffHunk {
                    header: "@@ -1,2 +1,3 @@".into(),
                    action: None,
                    lines: vec![
                        line(DiffLineKind::Context),
                        line(DiffLineKind::Addition),
                        line(DiffLineKind::Addition),
                    ],
                },
                DiffHunk {
                    header: "@@ -10 +11 @@".into(),
                    action: None,
                    lines: vec![line(DiffLineKind::Deletion), line(DiffLineKind::Addition)],
                },
            ],
        };
        assert_eq!(data.stats(), (3, 1));
    }

    #[test]
    fn split_hunk_header_separates_range_and_context() {
        let (range, ctx) = split_hunk_header("@@ -10,5 +12,7 @@ fn main() {");
        assert_eq!(range, "@@ -10,5 +12,7 @@");
        assert_eq!(ctx.as_deref(), Some("fn main() {"));

        let (range, ctx) = split_hunk_header("@@ -1 +1 @@");
        assert_eq!(range, "@@ -1 +1 @@");
        assert_eq!(ctx, None);
    }

    #[test]
    fn split_into_runs_alternates_normal_and_highlighted() {
        let runs = split_into_runs("abcdefghij", &[(2, 5), (7, 9)]);
        assert_eq!(
            runs,
            vec![
                ("ab".into(), false),
                ("cde".into(), true),
                ("fg".into(), false),
                ("hi".into(), true),
                ("j".into(), false),
            ]
        );
    }

    #[test]
    fn split_into_runs_skips_invalid_ranges() {
        // End past content length, non-boundary, and out-of-order.
        let runs = split_into_runs("ab", &[(0, 99), (1, 0)]);
        assert_eq!(runs, vec![("ab".into(), false)]);
    }
}
