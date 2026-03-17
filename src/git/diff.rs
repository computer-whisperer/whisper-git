//! Diff parsing, intra-line highlighting, and per-commit/working-file diff computation.

use anyhow::{Context, Result};
use git2::{Diff, Oid};

use super::GitRepo;

/// Byte offset ranges for intra-line diff highlighting
type DiffRanges = (Vec<(usize, usize)>, Vec<(usize, usize)>);

/// A file changed in a diff, with its hunks
#[derive(Clone, Debug)]
pub struct DiffFile {
    pub path: String,
    pub hunks: Vec<DiffHunk>,
    pub additions: usize,
    pub deletions: usize,
}

impl DiffFile {
    /// Build a DiffFile from a path and hunks, computing addition/deletion counts.
    pub fn from_hunks(path: String, hunks: Vec<DiffHunk>) -> Self {
        let additions = hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.origin == '+')
            .count();
        let deletions = hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.origin == '-')
            .count();
        Self {
            path,
            hunks,
            additions,
            deletions,
        }
    }
}

/// A hunk within a diff file
#[derive(Clone, Debug)]
pub struct DiffHunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

/// A single line in a diff hunk
#[derive(Clone, Debug)]
pub struct DiffLine {
    pub origin: char,
    pub content: String,
    pub old_lineno: Option<u32>,
    pub new_lineno: Option<u32>,
    /// Byte ranges within `content` that represent intra-line changes (word-level highlight).
    /// Empty means the entire line is changed (no paired line found for comparison).
    pub highlight_ranges: Vec<(usize, usize)>,
}

impl GitRepo {
    /// Get the diff for a commit compared to its first parent
    pub fn diff_for_commit(&self, oid: Oid) -> Result<Vec<DiffFile>> {
        let commit = self
            .repo
            .find_commit(oid)
            .context("Failed to find commit")?;
        let tree = commit.tree().context("Failed to get commit tree")?;

        let parent_tree = if commit.parent_count() > 0 {
            let parent = commit.parent(0).context("Failed to get parent commit")?;
            Some(parent.tree().context("Failed to get parent tree")?)
        } else {
            None
        };

        let diff = self
            .repo
            .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)
            .context("Failed to compute diff")?;

        parse_diff(&diff)
    }

    /// Get the diff hunks for a working directory file (staged or unstaged)
    pub fn diff_working_file(&self, path: &str, staged: bool) -> Result<Vec<DiffHunk>> {
        let mut opts = git2::DiffOptions::new();
        opts.pathspec(path);

        let diff = if staged {
            let head = self.repo.head().context("Failed to get HEAD")?;
            let head_tree = head.peel_to_tree().context("Failed to get HEAD tree")?;
            self.repo.diff_tree_to_index(
                Some(&head_tree),
                Some(&self.repo.index()?),
                Some(&mut opts),
            )?
        } else {
            self.repo.diff_index_to_workdir(None, Some(&mut opts))?
        };

        let files = parse_diff(&diff)?;
        Ok(files.into_iter().flat_map(|f| f.hunks).collect())
    }

    /// Get diff for a specific file in a commit
    pub fn diff_file_in_commit(&self, oid: Oid, file_path: &str) -> Result<Vec<DiffFile>> {
        let commit = self
            .repo
            .find_commit(oid)
            .context("Failed to find commit")?;
        let tree = commit.tree().context("Failed to get commit tree")?;

        let parent_tree = if commit.parent_count() > 0 {
            let parent = commit.parent(0).context("Failed to get parent commit")?;
            Some(parent.tree().context("Failed to get parent tree")?)
        } else {
            None
        };

        let mut opts = git2::DiffOptions::new();
        opts.pathspec(file_path);

        let diff = self
            .repo
            .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))
            .context("Failed to compute diff")?;

        parse_diff(&diff)
    }
}

/// Compute intra-line highlight ranges for paired add/remove lines within hunks.
/// Finds consecutive `-` then `+` line pairs and highlights the differing byte ranges.
fn compute_intra_line_highlights(files: &mut [DiffFile]) {
    for file in files.iter_mut() {
        for hunk in &mut file.hunks {
            // Find paired -/+ line runs within the hunk
            let len = hunk.lines.len();
            let mut i = 0;
            while i < len {
                // Collect a run of '-' lines followed by a run of '+' lines
                let del_start = i;
                while i < len && hunk.lines[i].origin == '-' {
                    i += 1;
                }
                let del_end = i;

                let add_start = i;
                while i < len && hunk.lines[i].origin == '+' {
                    i += 1;
                }
                let add_end = i;

                let del_count = del_end - del_start;
                let add_count = add_end - add_start;

                // Only compute highlights if we have paired lines
                if del_count > 0 && add_count > 0 {
                    let pair_count = del_count.min(add_count);
                    for j in 0..pair_count {
                        let del_idx = del_start + j;
                        let add_idx = add_start + j;
                        let (del_ranges, add_ranges) =
                            diff_chars(&hunk.lines[del_idx].content, &hunk.lines[add_idx].content);
                        hunk.lines[del_idx].highlight_ranges = del_ranges;
                        hunk.lines[add_idx].highlight_ranges = add_ranges;
                    }
                }

                // Skip context lines
                if i == del_end && i == add_start {
                    i += 1;
                }
            }
        }
    }
}

/// Compute the differing byte ranges between two strings.
/// Returns (old_ranges, new_ranges) where each range is a (start, end) byte offset
/// into the respective string's content (excluding trailing newline).
fn diff_chars(old: &str, new: &str) -> DiffRanges {
    let old = old.trim_end_matches('\n');
    let new = new.trim_end_matches('\n');

    let old_bytes = old.as_bytes();
    let new_bytes = new.as_bytes();

    // Find common prefix length
    let prefix_len = old_bytes
        .iter()
        .zip(new_bytes.iter())
        .take_while(|(a, b)| a == b)
        .count();

    // Find common suffix length (not overlapping with prefix)
    let old_remaining = old_bytes.len() - prefix_len;
    let new_remaining = new_bytes.len() - prefix_len;
    let suffix_len = old_bytes[prefix_len..]
        .iter()
        .rev()
        .zip(new_bytes[prefix_len..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count()
        .min(old_remaining)
        .min(new_remaining);

    let old_diff_end = old_bytes.len() - suffix_len;
    let new_diff_end = new_bytes.len() - suffix_len;

    // If the entire line changed or nothing changed, return empty (render as full-line highlight)
    if prefix_len == 0 && suffix_len == 0 {
        return (Vec::new(), Vec::new());
    }
    if prefix_len >= old_diff_end && prefix_len >= new_diff_end {
        // Lines are identical
        return (Vec::new(), Vec::new());
    }

    // Snap byte offsets to char boundaries so slicing never panics on multi-byte UTF-8
    fn snap_to_char_boundaries(s: &str, start: usize, end: usize) -> (usize, usize) {
        // Move start backward to the nearest char boundary
        let mut s_start = start;
        while s_start > 0 && !s.is_char_boundary(s_start) {
            s_start -= 1;
        }
        // Move end forward to the nearest char boundary
        let mut s_end = end;
        while s_end < s.len() && !s.is_char_boundary(s_end) {
            s_end += 1;
        }
        (s_start, s_end)
    }

    let old_ranges = if prefix_len < old_diff_end {
        let (s, e) = snap_to_char_boundaries(old, prefix_len, old_diff_end);
        vec![(s, e)]
    } else {
        Vec::new()
    };
    let new_ranges = if prefix_len < new_diff_end {
        let (s, e) = snap_to_char_boundaries(new, prefix_len, new_diff_end);
        vec![(s, e)]
    } else {
        Vec::new()
    };

    (old_ranges, new_ranges)
}

/// Parse a git2 Diff into structured DiffFile/DiffHunk/DiffLine data.
pub(super) fn parse_diff(diff: &Diff) -> Result<Vec<DiffFile>> {
    let mut files: Vec<DiffFile> = Vec::new();

    diff.print(git2::DiffFormat::Patch, |delta, hunk, line| {
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        // Create a new file entry if the path changed
        let need_new_file = files
            .last()
            .map(|f: &DiffFile| f.path != path)
            .unwrap_or(true);
        if need_new_file {
            files.push(DiffFile {
                path,
                hunks: Vec::new(),
                additions: 0,
                deletions: 0,
            });
        }

        let file = files.last_mut().unwrap();
        let origin = line.origin();

        match origin {
            'F' | 'H' => {
                // File header or hunk header
                if origin == 'H' {
                    let header = hunk
                        .map(|h| String::from_utf8_lossy(h.header()).trim_end().to_string())
                        .unwrap_or_default();
                    file.hunks.push(DiffHunk {
                        header,
                        lines: Vec::new(),
                    });
                }
            }
            '+' | '-' | ' ' => {
                match origin {
                    '+' => file.additions += 1,
                    '-' => file.deletions += 1,
                    _ => {}
                }
                // Create default hunk if none exists yet
                if file.hunks.is_empty() {
                    file.hunks.push(DiffHunk {
                        header: String::new(),
                        lines: Vec::new(),
                    });
                }
                if let Some(hunk) = file.hunks.last_mut() {
                    hunk.lines.push(DiffLine {
                        origin,
                        content: String::from_utf8_lossy(line.content()).to_string(),
                        old_lineno: line.old_lineno(),
                        new_lineno: line.new_lineno(),
                        highlight_ranges: Vec::new(),
                    });
                }
            }
            _ => {}
        }
        true
    })?;

    compute_intra_line_highlights(&mut files);
    Ok(files)
}
