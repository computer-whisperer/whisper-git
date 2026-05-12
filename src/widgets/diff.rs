//! Unified-diff viewer widget — Github-shaped: colored gutter strip
//! per line, subtle row-level wash, optional word-level intra-line
//! highlights, and per-hunk action buttons.
//!
//! Pure data-in: the widget takes a [`DiffData`] value and renders it.
//! Whisper-git's `diff_view::diff_view(tab)` is the thin adapter that
//! resolves the right hunks (working-tree vs commit) and builds the
//! input. Once the API settles this module is a candidate for moving
//! upstream into aetna's catalog.
//!
//! The body is a single [`virtual_list_dyn`]: hunks are flattened into
//! a row stream where hunk-header rows interleave with line rows, and
//! only the visible window is materialized per frame. This keeps a
//! 5,000-line diff cheap to scroll. The flat stream also matches
//! Github's hunk-header-as-band layout — there is no per-hunk card.

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
/// follow. `actions` are optional buttons rendered in the hunk header;
/// the app pre-routes the click keys.
#[derive(Clone, Debug)]
pub struct DiffHunk {
    /// The full `@@` header line as emitted by libgit2 (e.g.
    /// `@@ -10,5 +10,7 @@ fn main()`).
    pub header: String,
    pub lines: Vec<DiffLine>,
    pub actions: Vec<DiffHunkAction>,
}

#[derive(Clone, Debug)]
pub struct DiffHunkAction {
    pub label: String,
    pub key: String,
    pub tooltip: Option<String>,
    pub destructive: bool,
}

/// Top-level diff payload. `title` is the path; `badge` is the
/// secondary tag shown in the header (e.g. `staged`, `unstaged`, or a
/// commit short SHA).
#[derive(Clone, Debug)]
pub struct DiffData {
    pub title: String,
    pub badge: Option<String>,
    pub hunks: Vec<DiffHunk>,
    pub mode: DiffMode,
    /// Routed key for the mode-toggle button in the file header.
    /// `None` hides the toggle (e.g., the host doesn't want a button
    /// because it provides its own UI for switching).
    pub mode_toggle_key: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum DiffMode {
    /// Single column with `+` / `-` lines stacked, Github default.
    #[default]
    Unified,
    /// Two columns (old | new) with paired changes side-by-side.
    Split,
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
/// Estimated row height for `virtual_list_dyn`. Most rows are single
/// mono lines (~18px) — hunk-header rows are taller (~32px) but rare.
/// The library measures actual heights as rows enter the viewport and
/// caches them, so the estimate just picks the initial scrollbar
/// thumb size.
const EST_ROW_HEIGHT: f32 = 22.0;
/// Row-level wash for changed lines (`+` / `-`). Subtle: the diff
/// should still read as text, not as alternating colored stripes.
const ROW_BG_ALPHA: u8 = 48;
/// The line-number gutter sits on top of the row wash with a slightly
/// darker overlay — Github-style "this column is the gutter, not
/// content."
const GUTTER_TINT_ALPHA: u8 = 40;
const SCROLLBAR_GUTTER: f32 = tokens::SCROLLBAR_THUMB_WIDTH_ACTIVE + tokens::SCROLLBAR_TRACK_INSET;
/// Brighter wash painted under the changed bytes within a line, on
/// top of the row wash. Mirrors `<mark>` over the line's colored bg.
const HIGHLIGHT_BG_ALPHA: u8 = 130;

#[track_caller]
pub fn diff(data: &DiffData) -> El {
    let (adds, dels) = data.stats();

    let mut header_children: Vec<El> = vec![text(data.title.clone()).label(), spacer()];
    if !data.hunks.is_empty() {
        // .caption() applies TEXT_XS metrics but resets font_mono to
        // false (caption is intentionally proportional). Apply .mono()
        // after so the JetBrains Mono routing wins back.
        header_children.push(
            text(format!("+{adds}"))
                .caption()
                .mono()
                .text_color(tokens::SUCCESS),
        );
        header_children.push(
            text(format!("-{dels}"))
                .caption()
                .mono()
                .text_color(tokens::DESTRUCTIVE),
        );
    }
    if let Some(b) = data.badge.as_ref() {
        header_children.push(badge(b.clone()).muted());
    }
    if let Some(key) = data.mode_toggle_key.as_ref() {
        header_children.push(mode_toggle_button(data.mode, key));
    }
    let header_row = row(header_children)
        .gap(tokens::SPACE_2)
        .align(Align::Center);

    let body: El = if data.hunks.is_empty() {
        column([text("(no changes)").caption().muted()]).padding(tokens::SPACE_4)
    } else {
        let rows = flatten_rows(&data.hunks, data.mode);
        virtual_list_dyn(rows.len(), EST_ROW_HEIGHT, move |i| {
            column([build_diff_row(&rows[i], i)])
                .width(Size::Fill(1.0))
                .padding(Sides {
                    left: 0.0,
                    right: SCROLLBAR_GUTTER,
                    top: 0.0,
                    bottom: 0.0,
                })
        })
        .key("diff:scroll")
        .height(Size::Fill(1.0))
    };

    card([
        card_header([header_row])
            .padding(Sides::xy(tokens::SPACE_4, tokens::SPACE_2))
            .fill(tokens::MUTED),
        card_content([body])
            .padding(Sides {
                left: 0.0,
                right: 0.0,
                top: 0.0,
                bottom: tokens::RING_WIDTH,
            })
            .height(Size::Fill(1.0)),
    ])
    .height(Size::Fill(1.0))
    .width(Size::Fill(1.0))
}

/// One row in the flattened hunk stream. Hunk headers interleave with
/// line rows so the entire body is a single virtual list.
#[derive(Clone)]
enum DiffRow {
    HunkHeader {
        header: String,
        actions: Vec<DiffHunkAction>,
    },
    UnifiedLine(DiffLine),
    SplitPair(PairedRow),
}

fn flatten_rows(hunks: &[DiffHunk], mode: DiffMode) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    for hunk in hunks {
        rows.push(DiffRow::HunkHeader {
            header: hunk.header.clone(),
            actions: hunk.actions.clone(),
        });
        match mode {
            DiffMode::Unified => {
                for l in &hunk.lines {
                    rows.push(DiffRow::UnifiedLine(l.clone()));
                }
            }
            DiffMode::Split => {
                for p in pair_lines(&hunk.lines) {
                    rows.push(DiffRow::SplitPair(p));
                }
            }
        }
    }
    rows
}

fn build_diff_row(row: &DiffRow, idx: usize) -> El {
    let key = format!("diff:row:{idx}");
    match row {
        DiffRow::HunkHeader { header, actions } => hunk_header_row(header, actions).key(key),
        DiffRow::UnifiedLine(line) => unified_line_row(line).key(key),
        DiffRow::SplitPair(pair) => split_pair_row(pair).key(key),
    }
}

fn mode_toggle_button(mode: DiffMode, key: &str) -> El {
    let (label, tip) = match mode {
        DiffMode::Unified => ("Split", "Switch to side-by-side view"),
        DiffMode::Split => ("Unified", "Switch to single-column view"),
    };
    button(label.to_string())
        .key(key.to_string())
        .ghost()
        .small()
        .tooltip(tip.to_string())
}

/// One hunk-header band rendered as a single flat row for the
/// virtual list. Carries a MUTED fill that visually separates one
/// hunk's lines from the next — replaces what the old per-hunk card
/// boundary used to do.
fn hunk_header_row(header: &str, actions: &[DiffHunkAction]) -> El {
    let mut children: Vec<El> = Vec::with_capacity(3);
    let (range, context) = split_hunk_header(header);
    children.push(text(range).code().text_color(tokens::INFO));
    if let Some(ctx) = context {
        children.push(text(ctx).code().muted().ellipsis().width(Size::Fill(1.0)));
    } else {
        children.push(spacer());
    }
    for act in actions {
        let mut btn = button(act.label.clone()).key(act.key.clone()).ghost();
        if act.destructive {
            btn = btn.destructive();
        }
        if let Some(tip) = act.tooltip.as_ref() {
            btn = btn.tooltip(tip.clone());
        }
        children.push(btn);
    }
    row(children)
        .width(Size::Fill(1.0))
        .gap(tokens::SPACE_2)
        .align(Align::Center)
        .padding(Sides::xy(tokens::SPACE_2, tokens::SPACE_1))
        .fill(tokens::MUTED)
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

/// One row in unified mode: [old_no | new_no | content], a single
/// row tinted by the line kind.
fn unified_line_row(line: &DiffLine) -> El {
    let (row_bg, gutter_overlay) = backgrounds_for(line.kind);

    let old_no = line.old_lineno.map(|n| n.to_string()).unwrap_or_default();
    let new_no = line.new_lineno.map(|n| n.to_string()).unwrap_or_default();

    let gutter = row([lineno_col(old_no), lineno_col(new_no)])
        .fill(gutter_overlay)
        .align(Align::Center);

    let row_el = row([gutter, line_content(line)])
        .align(Align::Center)
        .width(Size::Fill(1.0));

    if let Some(bg) = row_bg {
        row_el.fill(bg)
    } else {
        row_el
    }
}

fn lineno_col(s: String) -> El {
    // .caption() is the right role for small annotations like line
    // numbers, but it explicitly resets font_mono to false (caption
    // text is intentionally proportional). Chain .mono() after so
    // the JetBrains Mono routing wins back — without that, line
    // numbers render in Inter and stop aligning vertically.
    text(s)
        .caption()
        .mono()
        .muted()
        .nowrap_text()
        .text_align(TextAlign::End)
        .width(Size::Fixed(LINENO_COL_WIDTH))
        .padding(Sides::xy(tokens::SPACE_2, 0.0))
}

fn backgrounds_for(kind: DiffLineKind) -> (Option<Color>, Color) {
    match kind {
        DiffLineKind::Addition => (
            Some(tokens::SUCCESS.with_alpha(ROW_BG_ALPHA)),
            tokens::SUCCESS.with_alpha(GUTTER_TINT_ALPHA),
        ),
        DiffLineKind::Deletion => (
            Some(tokens::DESTRUCTIVE.with_alpha(ROW_BG_ALPHA)),
            tokens::DESTRUCTIVE.with_alpha(GUTTER_TINT_ALPHA),
        ),
        DiffLineKind::Context => (None, tokens::MUTED.with_alpha(GUTTER_TINT_ALPHA)),
    }
}

/// One row in split mode: [old_lineno | old_content | new_lineno | new_content].
/// Either side may be `None` (orphan add/delete with no paired counterpart).
fn split_pair_row(pair: &PairedRow) -> El {
    let left = side_half(pair.left.as_ref(), Side::Left);
    let right = side_half(pair.right.as_ref(), Side::Right);
    row([left, right])
        .align(Align::Stretch)
        .width(Size::Fill(1.0))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Side {
    Left,
    Right,
}

/// Render one half of a split row. `None` is an orphan placeholder
/// (the other side has a +/- with no paired counterpart) — we paint
/// the row tint without content so the eye sees "this side is empty
/// here, the change is on the other side."
fn side_half(line: Option<&DiffLine>, side: Side) -> El {
    let lineno = line
        .and_then(|l| match side {
            Side::Left => l.old_lineno,
            Side::Right => l.new_lineno,
        })
        .map(|n| n.to_string())
        .unwrap_or_default();

    let kind_for_tint = line.map(|l| l.kind).unwrap_or(DiffLineKind::Context);
    // Empty-orphan halves get the *opposite* side's tint so a
    // missing left (paired with an addition on the right) shows as
    // a subtle gray-equivalent missing slot rather than a fake
    // green/red. We use a slightly stronger MUTED tint to mark "no
    // line here."
    let (row_bg, gutter_overlay) = if line.is_some() {
        backgrounds_for(kind_for_tint)
    } else {
        let muted = tokens::MUTED.with_alpha(ROW_BG_ALPHA);
        let muted_gutter = tokens::MUTED.with_alpha(GUTTER_TINT_ALPHA + 24);
        (Some(muted), muted_gutter)
    };

    let gutter = row([lineno_col(lineno)])
        .fill(gutter_overlay)
        .align(Align::Center);

    let content = match line {
        Some(l) => line_content(l),
        None => text(String::new())
            .mono()
            .nowrap_text()
            .padding(Sides::xy(tokens::SPACE_2, 0.0))
            .width(Size::Fill(1.0)),
    };

    // `.clip()` truncates the content at the half boundary. Without
    // it, `nowrap_text`'s min-content width forces long lines to
    // overflow past the half's allocated rect and bleed into the
    // opposite column. To see a clipped line in full, the user
    // switches to Unified.
    let half = row([gutter, content])
        .align(Align::Center)
        .width(Size::Fill(1.0))
        .clip();

    if let Some(bg) = row_bg {
        half.fill(bg)
    } else {
        half
    }
}

#[derive(Clone, Debug)]
struct PairedRow {
    left: Option<DiffLine>,
    right: Option<DiffLine>,
}

/// Pair lines for split rendering. Context lines pair with themselves.
/// Consecutive runs of `-` and `+` lines pair index-by-index; if one
/// run is longer, the extra lines pair with `None` on the other side.
///
/// This is the standard "myers-ish naive pairing" most diff viewers
/// use; it doesn't try to find the best LCS within a hunk. For real
/// code review the result is usually right because libgit2's
/// `highlight_ranges` already nudges nearby `-` / `+` lines into the
/// expected pairing.
fn pair_lines(lines: &[DiffLine]) -> Vec<PairedRow> {
    let mut out = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        match lines[i].kind {
            DiffLineKind::Context => {
                out.push(PairedRow {
                    left: Some(lines[i].clone()),
                    right: Some(lines[i].clone()),
                });
                i += 1;
            }
            DiffLineKind::Addition | DiffLineKind::Deletion => {
                let mut deletions: Vec<DiffLine> = Vec::new();
                let mut additions: Vec<DiffLine> = Vec::new();
                while i < lines.len() {
                    match lines[i].kind {
                        DiffLineKind::Deletion => deletions.push(lines[i].clone()),
                        DiffLineKind::Addition => additions.push(lines[i].clone()),
                        DiffLineKind::Context => break,
                    }
                    i += 1;
                }
                let n = deletions.len().max(additions.len());
                for j in 0..n {
                    out.push(PairedRow {
                        left: deletions.get(j).cloned(),
                        right: additions.get(j).cloned(),
                    });
                }
            }
        }
    }
    out
}

/// Render a line's content. When the line carries `highlights`,
/// the line is composed as `text_runs([..text(span).background(hot)..])`
/// so the whole line shapes as a single inline run and the brighter
/// highlight bg paints tightly under the marked glyphs (per the
/// `Kind::Inlines` contract). The previous row-of-text-leaves shape
/// gave each segment its own El bbox, which left visible margins
/// between consecutive runs.
///
/// Text stays FOREGROUND across the board — the row bg + word
/// highlight bg carry the color signal, and Github-style colored
/// text on tinted bg hurts readability for context-rich diffs.
fn line_content(line: &DiffLine) -> El {
    let highlight_bg = match line.kind {
        DiffLineKind::Addition => Some(tokens::SUCCESS.with_alpha(HIGHLIGHT_BG_ALPHA)),
        DiffLineKind::Deletion => Some(tokens::DESTRUCTIVE.with_alpha(HIGHLIGHT_BG_ALPHA)),
        DiffLineKind::Context => None,
    };

    let trimmed = line.content.trim_end_matches('\n');
    let runs = split_into_runs(trimmed, &line.highlights);
    if runs.len() == 1 || highlight_bg.is_none() {
        // No highlights (or context line) — single shaped span.
        // `ellipsis` so over-long lines (typical in split mode where
        // each half is narrower than the gutter+content needs) get a
        // "…" hint AND the lint understands the truncation as
        // intentional. Unified mode leaves more room, so the
        // ellipsis only fires on genuinely long lines.
        return text(trimmed.to_string())
            .mono()
            .nowrap_text()
            .ellipsis()
            .padding(Sides::xy(tokens::SPACE_2, 0.0))
            .width(Size::Fill(1.0));
    }
    let bg = highlight_bg.expect("checked above");
    let children: Vec<El> = runs
        .into_iter()
        .map(|(span, hot)| {
            let t = text(span).mono();
            if hot { t.background(bg) } else { t }
        })
        .collect();
    text_runs(children)
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
            mode: DiffMode::default(),
            mode_toggle_key: None,
            hunks: vec![
                DiffHunk {
                    header: "@@ -1,2 +1,3 @@".into(),
                    actions: Vec::new(),
                    lines: vec![
                        line(DiffLineKind::Context),
                        line(DiffLineKind::Addition),
                        line(DiffLineKind::Addition),
                    ],
                },
                DiffHunk {
                    header: "@@ -10 +11 @@".into(),
                    actions: Vec::new(),
                    lines: vec![line(DiffLineKind::Deletion), line(DiffLineKind::Addition)],
                },
            ],
        };
        assert_eq!(data.stats(), (3, 1));
    }

    fn line(kind: DiffLineKind, content: &str) -> DiffLine {
        DiffLine {
            kind,
            content: content.into(),
            old_lineno: None,
            new_lineno: None,
            highlights: Vec::new(),
        }
    }

    #[test]
    fn pair_lines_pairs_consecutive_dels_with_adds() {
        let lines = vec![
            line(DiffLineKind::Context, "ctx1"),
            line(DiffLineKind::Deletion, "old1"),
            line(DiffLineKind::Deletion, "old2"),
            line(DiffLineKind::Addition, "new1"),
            line(DiffLineKind::Addition, "new2"),
            line(DiffLineKind::Context, "ctx2"),
        ];
        let pairs = pair_lines(&lines);
        assert_eq!(pairs.len(), 4);
        // ctx pairs with itself.
        assert_eq!(pairs[0].left.as_ref().unwrap().content, "ctx1");
        assert_eq!(pairs[0].right.as_ref().unwrap().content, "ctx1");
        // The 2-deletion / 2-addition run pairs index-by-index.
        assert_eq!(pairs[1].left.as_ref().unwrap().content, "old1");
        assert_eq!(pairs[1].right.as_ref().unwrap().content, "new1");
        assert_eq!(pairs[2].left.as_ref().unwrap().content, "old2");
        assert_eq!(pairs[2].right.as_ref().unwrap().content, "new2");
        // Trailing context.
        assert_eq!(pairs[3].left.as_ref().unwrap().content, "ctx2");
        assert_eq!(pairs[3].right.as_ref().unwrap().content, "ctx2");
    }

    #[test]
    fn pair_lines_orphans_extra_dels_or_adds() {
        // 1 deletion + 3 additions — the first addition pairs with
        // the deletion, the other two are orphans (left=None).
        let lines = vec![
            line(DiffLineKind::Deletion, "old"),
            line(DiffLineKind::Addition, "new1"),
            line(DiffLineKind::Addition, "new2"),
            line(DiffLineKind::Addition, "new3"),
        ];
        let pairs = pair_lines(&lines);
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[0].left.as_ref().unwrap().content, "old");
        assert_eq!(pairs[0].right.as_ref().unwrap().content, "new1");
        assert!(pairs[1].left.is_none());
        assert_eq!(pairs[1].right.as_ref().unwrap().content, "new2");
        assert!(pairs[2].left.is_none());
        assert_eq!(pairs[2].right.as_ref().unwrap().content, "new3");
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
