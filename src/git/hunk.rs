//! Hunk-level staging, unstaging, and discarding operations.

use anyhow::{Context, Result};
use std::path::Path;

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

    /// Whether the (freshly reloaded) index has a stage-0 entry for `path`.
    fn index_has_path(&self, path: &str) -> bool {
        let Ok(mut index) = self.repo.index() else {
            return false;
        };
        let _ = index.read(false);
        index.get_path(Path::new(path), 0).is_some()
    }

    /// Whether HEAD's tree contains `path`.
    fn head_has_path(&self, path: &str) -> bool {
        self.repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_tree().ok())
            .is_some_and(|t| t.get_path(Path::new(path)).is_ok())
    }

    /// git file mode string for a new-file patch header, read from the
    /// working copy: `120000` symlink, `100755` executable, else `100644`.
    fn new_file_mode(&self, path: &str) -> &'static str {
        let Some(md) = self
            .workdir()
            .and_then(|wd| wd.join(path).symlink_metadata().ok())
        else {
            return "100644";
        };
        if md.file_type().is_symlink() {
            return "120000";
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if md.permissions().mode() & 0o111 != 0 {
                return "100755";
            }
        }
        "100644"
    }

    /// Apply a hunk patch to the index. When `reverse` is true the patch is
    /// applied in reverse (unstage); when false it stages the hunk.
    fn apply_hunk_patch(&self, file_path: &str, hunk_header: &str, reverse: bool) -> Result<()> {
        let hunks = self.diff_working_file(file_path, reverse)?;
        let hunk = Self::find_hunk(&hunks, hunk_header)?;

        // A hunk from a file-*creation* diff (untracked file when
        // staging, INDEX_NEW file when unstaging) must be shaped as a
        // git new-file patch. With plain `--- a/path` headers git
        // apply still exits 0 but does the wrong thing: forward
        // application drops the file mode (exec bit), and reverse
        // application truncates the file to zero bytes instead of
        // removing it.
        let is_creation = if reverse {
            !self.head_has_path(file_path)
        } else {
            !self.index_has_path(file_path)
        };
        let new_file_mode = is_creation.then(|| self.new_file_mode(file_path));
        let patch = build_hunk_patch(file_path, hunk, new_file_mode);
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

        // See apply_hunk_patch: reverse-applying an untracked file's
        // creation hunk must *remove* the file, which needs new-file
        // patch headers (plain headers truncate it to zero bytes).
        let new_file_mode =
            (!self.index_has_path(file_path)).then(|| self.new_file_mode(file_path));
        let patch = build_hunk_patch(file_path, hunk, new_file_mode);
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

/// Build a minimal unified-diff patch for a single hunk. Pass
/// `new_file_mode` when the hunk creates the file — the patch then
/// carries git's extended new-file headers (`diff --git`, `new file
/// mode`, `--- /dev/null`) so mode is preserved on apply and reverse
/// application deletes the file.
fn build_hunk_patch(path: &str, hunk: &DiffHunk, new_file_mode: Option<&str>) -> String {
    let mut patch = String::new();
    if let Some(mode) = new_file_mode {
        patch.push_str(&format!("diff --git a/{path} b/{path}\n"));
        patch.push_str(&format!("new file mode {mode}\n"));
        patch.push_str("--- /dev/null\n");
    } else {
        patch.push_str(&format!("--- a/{}\n", path));
    }
    patch.push_str(&format!("+++ b/{}\n", path));
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

    /// Regression: untracked-file hunks were emitted with plain
    /// `--- a/path` headers; `git apply` exits 0 on those but drops
    /// the exec bit when staging. A creation hunk must stage the
    /// file's content AND mode.
    #[cfg(unix)]
    #[test]
    fn stage_hunk_creates_index_entry_with_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_repo("hunk-untracked-stage");
        commit_file_helper(&dir);
        let script = dir.join("run.sh");
        fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let repo = GitRepo::open(&dir).expect("open repo");
        let hunks = repo.diff_working_file("run.sh", false).expect("diff");
        assert_eq!(hunks.len(), 1);
        repo.stage_hunk("run.sh", &hunks[0].header)
            .expect("stage untracked hunk");

        let entry = run_git(&dir, &["ls-files", "-s", "run.sh"]);
        assert!(
            entry.starts_with("100755"),
            "exec bit must be staged, got: {entry}"
        );
        assert_eq!(run_git(&dir, &["show", ":run.sh"]), "#!/bin/sh\necho hi");

        let _ = fs::remove_dir_all(&dir);
    }

    /// Regression: discarding an untracked file's hunk truncated the
    /// file to zero bytes (git apply reverse on a plain-header patch)
    /// instead of removing it. With new-file headers, reverse
    /// application deletes the file — matching whole-file discard.
    #[test]
    fn discard_hunk_removes_untracked_file() {
        let dir = temp_repo("hunk-untracked-discard");
        commit_file_helper(&dir);
        fs::write(dir.join("scratch.txt"), "temp\n").unwrap();

        let repo = GitRepo::open(&dir).expect("open repo");
        let hunks = repo.diff_working_file("scratch.txt", false).expect("diff");
        assert_eq!(hunks.len(), 1);
        repo.discard_hunk("scratch.txt", &hunks[0].header)
            .expect("discard untracked hunk");

        assert!(
            !dir.join("scratch.txt").exists(),
            "discard must remove the untracked file, not truncate it"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// Unstaging the only hunk of an INDEX_NEW file must remove the
    /// index entry entirely, not leave a zero-byte blob staged.
    #[test]
    fn unstage_hunk_removes_index_new_entry() {
        let dir = temp_repo("hunk-untracked-unstage");
        commit_file_helper(&dir);
        fs::write(dir.join("added.txt"), "fresh\n").unwrap();
        run_git(&dir, &["add", "added.txt"]);

        let repo = GitRepo::open(&dir).expect("open repo");
        let hunks = repo.diff_working_file("added.txt", true).expect("diff");
        assert_eq!(hunks.len(), 1);
        repo.unstage_hunk("added.txt", &hunks[0].header)
            .expect("unstage INDEX_NEW hunk");

        assert_eq!(
            run_git(&dir, &["ls-files", "--", "added.txt"]),
            "",
            "index entry must be gone"
        );
        assert_eq!(
            fs::read_to_string(dir.join("added.txt")).unwrap(),
            "fresh\n",
            "workdir copy must be untouched"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    fn commit_file_helper(dir: &Path) {
        fs::write(dir.join("base.txt"), "base\n").unwrap();
        run_git(dir, &["add", "base.txt"]);
        run_git(dir, &["commit", "-m", "base"]);
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
