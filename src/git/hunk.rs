//! Hunk-level staging, unstaging, and discarding operations.

use anyhow::{Context, Result};

use super::GitRepo;
use super::diff::DiffHunk;

impl GitRepo {
    /// Stage a single hunk from a working-directory file by building a minimal
    /// unified-diff patch and applying it to the index via `git apply --cached`.
    pub fn stage_hunk(&self, file_path: &str, hunk_index: usize) -> Result<()> {
        self.apply_hunk_patch(file_path, hunk_index, false)
    }

    /// Unstage a single hunk from the index by building a reverse patch and applying it.
    pub fn unstage_hunk(&self, file_path: &str, hunk_index: usize) -> Result<()> {
        self.apply_hunk_patch(file_path, hunk_index, true)
    }

    /// Apply a hunk patch to the index. When `reverse` is true the patch is
    /// applied in reverse (unstage); when false it stages the hunk.
    fn apply_hunk_patch(&self, file_path: &str, hunk_index: usize, reverse: bool) -> Result<()> {
        let hunks = self.diff_working_file(file_path, reverse)?;
        let hunk = hunks.get(hunk_index).ok_or_else(|| {
            anyhow::anyhow!(
                "Hunk index {} out of range (file has {} hunks)",
                hunk_index,
                hunks.len()
            )
        })?;

        let patch = build_hunk_patch(file_path, file_path, hunk);
        let workdir = self
            .workdir()
            .ok_or_else(|| anyhow::anyhow!("No working directory"))?;

        let mut args = vec!["apply", "--cached"];
        if reverse {
            args.push("--reverse");
        }
        args.extend(["--unidiff-zero", "-"]);

        let output = std::process::Command::new("git")
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .current_dir(workdir)
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(ref mut stdin) = child.stdin {
                    stdin.write_all(patch.as_bytes())?;
                }
                child.wait_with_output()
            })
            .with_context(|| {
                format!(
                    "Failed to run git apply{}",
                    if reverse { " --reverse" } else { "" }
                )
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let action = if reverse { "unstage" } else { "stage" };
            anyhow::bail!("Failed to {} hunk: {}", action, stderr);
        }
        Ok(())
    }

    /// Discard a single hunk from the working tree by applying the reverse patch
    /// directly to the working directory (no --cached).
    pub fn discard_hunk(&self, file_path: &str, hunk_index: usize) -> Result<()> {
        let hunks = self.diff_working_file(file_path, false)?;
        let hunk = hunks.get(hunk_index).ok_or_else(|| {
            anyhow::anyhow!(
                "Hunk index {} out of range (file has {} hunks)",
                hunk_index,
                hunks.len()
            )
        })?;

        let patch = build_hunk_patch(file_path, file_path, hunk);
        let workdir = self
            .workdir()
            .ok_or_else(|| anyhow::anyhow!("No working directory"))?;

        let output = std::process::Command::new("git")
            .args(["apply", "--reverse", "--unidiff-zero", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .current_dir(workdir)
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(ref mut stdin) = child.stdin {
                    stdin.write_all(patch.as_bytes())?;
                }
                child.wait_with_output()
            })
            .with_context(|| "Failed to run git apply --reverse")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to discard hunk: {}", stderr);
        }
        Ok(())
    }
}

/// Build a minimal unified-diff patch for a single hunk.
fn build_hunk_patch(old_path: &str, new_path: &str, hunk: &DiffHunk) -> String {
    let mut patch = String::new();
    patch.push_str(&format!("--- a/{}\n", old_path));
    patch.push_str(&format!("+++ b/{}\n", new_path));
    patch.push_str(&hunk.header);
    if !hunk.header.ends_with('\n') {
        patch.push('\n');
    }
    for line in &hunk.lines {
        patch.push(line.origin);
        patch.push_str(&line.content);
        if !line.content.ends_with('\n') {
            patch.push('\n');
        }
    }
    patch
}
