//! Hunk-level staging, unstaging, and discarding operations.

use anyhow::{Context, Result};

use super::GitRepo;
use super::diff::DiffHunk;

impl GitRepo {
    /// Stage a single hunk from a working-directory file by building a minimal
    /// unified-diff patch and applying it to the index via `git apply --cached`.
    pub fn stage_hunk(&self, file_path: &str, hunk_header: &str) -> Result<()> {
        self.apply_hunk_patch(file_path, hunk_header, false)
    }

    /// Unstage a single hunk from the index by building a reverse patch and applying it.
    pub fn unstage_hunk(&self, file_path: &str, hunk_header: &str) -> Result<()> {
        self.apply_hunk_patch(file_path, hunk_header, true)
    }

    /// Find the hunk matching `header` in a freshly computed diff.
    ///
    /// Hunks are addressed by their `@@` header rather than list index:
    /// the index the UI displayed can go stale (file edited between
    /// render and click), and a stale index silently selects the WRONG
    /// hunk. A stale header instead fails to match — the op errors out
    /// rather than staging or discarding something the user never saw.
    fn find_hunk<'a>(hunks: &'a [DiffHunk], header: &str) -> Result<&'a DiffHunk> {
        hunks.iter().find(|h| h.header == header).ok_or_else(|| {
            anyhow::anyhow!("The file changed since this diff was shown — try again")
        })
    }

    /// Apply a hunk patch to the index. When `reverse` is true the patch is
    /// applied in reverse (unstage); when false it stages the hunk.
    fn apply_hunk_patch(&self, file_path: &str, hunk_header: &str, reverse: bool) -> Result<()> {
        let hunks = self.diff_working_file(file_path, reverse)?;
        let hunk = Self::find_hunk(&hunks, hunk_header)?;

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
    pub fn discard_hunk(&self, file_path: &str, hunk_header: &str) -> Result<()> {
        let hunks = self.diff_working_file(file_path, false)?;
        let hunk = Self::find_hunk(&hunks, hunk_header)?;

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
        // An unterminated final line must carry git's marker, or
        // `git apply` rejects the hunk as not matching the file.
        if line.no_newline {
            patch.push_str("\\ No newline at end of file\n");
        }
    }
    patch
}

#[cfg(test)]
mod hunk_tests {
    use super::super::GitRepo;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn run_git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("failed to run git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn temp_repo(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("whisper-git-{name}-{unique}"));
        fs::create_dir_all(&dir).expect("create temp repo dir");
        run_git(&dir, &["init", "-b", "main"]);
        run_git(&dir, &["config", "user.name", "Test"]);
        run_git(&dir, &["config", "user.email", "test@example.com"]);
        dir
    }

    /// Commit `content` (verbatim — callers pass strings WITHOUT a
    /// trailing newline to exercise the EOFNL paths), then apply the
    /// edit and return the single resulting hunk's header.
    fn fixture(name: &str, committed: &str, edited: &str) -> (PathBuf, GitRepo, String) {
        let dir = temp_repo(name);
        fs::write(dir.join("a.txt"), committed).unwrap();
        run_git(&dir, &["add", "a.txt"]);
        run_git(&dir, &["commit", "-m", "initial"]);
        fs::write(dir.join("a.txt"), edited).unwrap();
        let repo = GitRepo::open(&dir).expect("open repo");
        let hunks = repo.diff_working_file("a.txt", false).expect("diff");
        assert_eq!(hunks.len(), 1, "fixture expects exactly one hunk");
        let header = hunks[0].header.clone();
        (dir, repo, header)
    }

    /// Regression: files without a trailing newline produced patches
    /// missing the "\ No newline at end of file" marker, which
    /// `git apply` rejects — hunk staging failed on every hunk
    /// touching such a file's last line. Covers markers on both sides
    /// (old and new unterminated).
    #[test]
    fn stage_hunk_handles_missing_trailing_newline() {
        let (dir, repo, header) = fixture("hunk-eofnl-stage", "one\ntwo", "one\ntwo!");
        repo.stage_hunk("a.txt", &header).expect("stage hunk");
        assert_eq!(
            run_git(&dir, &["show", ":a.txt"]),
            "one\ntwo!",
            "index must carry the staged edit"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Marker on the old side only: the committed file was
    /// unterminated, the edit adds the trailing newline.
    #[test]
    fn stage_hunk_handles_newline_added_at_eof() {
        let (dir, repo, header) = fixture("hunk-eofnl-add", "one\ntwo", "one\ntwo\n");
        repo.stage_hunk("a.txt", &header).expect("stage hunk");
        assert!(
            repo.diff_working_file("a.txt", false)
                .expect("diff")
                .is_empty(),
            "the whole edit was staged, so the unstaged diff must be empty"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Discard must reverse-apply cleanly to a working file without a
    /// trailing newline, restoring the committed content exactly.
    #[test]
    fn discard_hunk_restores_file_without_trailing_newline() {
        let (dir, repo, header) = fixture("hunk-eofnl-discard", "one\ntwo", "one\ntwo!");
        repo.discard_hunk("a.txt", &header).expect("discard hunk");
        assert_eq!(
            fs::read_to_string(dir.join("a.txt")).unwrap(),
            "one\ntwo",
            "workdir must be restored byte-exact (no trailing newline)"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Regression: hunk ops used to trust a list index computed at
    /// render time — if the file changed in between, the index
    /// silently selected the wrong hunk. Header addressing must fail
    /// instead of guessing.
    #[test]
    fn hunk_ops_reject_stale_header() {
        let (dir, repo, _header) = fixture("hunk-stale-header", "one\ntwo\n", "one\ntwo!\n");
        let err = repo
            .stage_hunk("a.txt", "@@ -99,1 +99,1 @@ stale")
            .expect_err("stale header must not apply");
        assert!(
            err.to_string().contains("changed since"),
            "unexpected error: {err}"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
