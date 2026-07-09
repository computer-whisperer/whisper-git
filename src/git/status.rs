//! Working directory status, file status classification, and staging operations.

use anyhow::{Context, Result};
use git2::{Status, StatusOptions};
use std::path::Path;

use super::GitRepo;

/// Working directory status
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkingDirStatus {
    pub staged: Vec<FileStatus>,
    pub unstaged: Vec<FileStatus>,
    /// Untracked (new) files not yet known to git
    pub untracked: Vec<FileStatus>,
    pub conflicted: Vec<FileStatus>,
}

impl WorkingDirStatus {
    pub fn total_files(&self) -> usize {
        self.staged.len() + self.unstaged.len() + self.untracked.len() + self.conflicted.len()
    }
}

/// Status of a single file
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStatus {
    pub path: String,
    pub status: FileStatusKind,
}

/// Kind of file status change
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatusKind {
    New,
    Modified,
    Deleted,
    Renamed,
    TypeChange,
    Conflicted,
}

impl FileStatusKind {
    fn from_index_status(status: Status) -> Self {
        if status.contains(Status::INDEX_NEW) {
            FileStatusKind::New
        } else if status.contains(Status::INDEX_MODIFIED) {
            FileStatusKind::Modified
        } else if status.contains(Status::INDEX_DELETED) {
            FileStatusKind::Deleted
        } else if status.contains(Status::INDEX_RENAMED) {
            FileStatusKind::Renamed
        } else {
            FileStatusKind::TypeChange
        }
    }

    fn from_wt_status(status: Status) -> Self {
        if status.contains(Status::WT_NEW) {
            FileStatusKind::New
        } else if status.contains(Status::WT_MODIFIED) {
            FileStatusKind::Modified
        } else if status.contains(Status::WT_DELETED) {
            FileStatusKind::Deleted
        } else if status.contains(Status::WT_RENAMED) {
            FileStatusKind::Renamed
        } else {
            FileStatusKind::TypeChange
        }
    }
}

/// Build a `WorkingDirStatus` from raw `git2::Statuses`.
/// Extracted as a free function so background threads can use it without a `GitRepo`.
pub fn working_dir_status_from_statuses(statuses: &git2::Statuses<'_>) -> WorkingDirStatus {
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();
    let mut conflicted = Vec::new();

    for entry in statuses.iter() {
        let path = entry.path().unwrap_or("").to_string();
        let status = entry.status();

        if status.contains(Status::CONFLICTED) {
            conflicted.push(FileStatus {
                path,
                status: FileStatusKind::Conflicted,
            });
            continue;
        }

        if status.intersects(
            Status::INDEX_NEW
                | Status::INDEX_MODIFIED
                | Status::INDEX_DELETED
                | Status::INDEX_RENAMED
                | Status::INDEX_TYPECHANGE,
        ) {
            staged.push(FileStatus {
                path: path.clone(),
                status: FileStatusKind::from_index_status(status),
            });
        }

        if status.contains(Status::WT_NEW) {
            untracked.push(FileStatus {
                path,
                status: FileStatusKind::New,
            });
        } else if status.intersects(
            Status::WT_MODIFIED | Status::WT_DELETED | Status::WT_RENAMED | Status::WT_TYPECHANGE,
        ) {
            unstaged.push(FileStatus {
                path,
                status: FileStatusKind::from_wt_status(status),
            });
        }
    }

    WorkingDirStatus {
        staged,
        unstaged,
        untracked,
        conflicted,
    }
}

/// Staged submodule-pointer (gitlink) changes — HEAD tree vs index,
/// gitlink entries only.
///
/// The status walk runs with `exclude_submodules`, which libgit2 maps
/// to submodule-ignore=all on *both* diff halves. That keeps the walk
/// out of submodule working directories (the recursion the option
/// exists to prevent), but it also hides a staged pointer change —
/// dead-ending the app's own "Update pointer" flow at "No staged
/// changes". A tree→index diff never opens a submodule workdir, so
/// re-adding just the staged half is cheap and can't recurse.
pub fn staged_gitlinks(repo: &git2::Repository) -> Vec<FileStatus> {
    // Reload the index if it changed on disk — this handle may be
    // long-lived and libgit2 caches the snapshot (same fix as
    // `GitRepo::commit`), whereas the status walk refreshes itself.
    let Ok(mut index) = repo.index() else {
        return Vec::new();
    };
    let _ = index.read(false);
    // Unborn HEAD → diff against the empty tree, so a gitlink staged
    // into a fresh repo still shows.
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let Ok(diff) = repo.diff_tree_to_index(head_tree.as_ref(), Some(&index), None) else {
        return Vec::new();
    };
    diff.deltas()
        .filter(|d| {
            // tree→index diffs include conflict entries as
            // Delta::Conflicted — an unresolved merge conflict on a
            // pointer is NOT a staged change (the status walk already
            // reports it under `conflicted`).
            d.status() != git2::Delta::Conflicted
                && (d.old_file().mode() == git2::FileMode::Commit
                    || d.new_file().mode() == git2::FileMode::Commit)
        })
        .map(|d| FileStatus {
            path: d
                .new_file()
                .path()
                .or_else(|| d.old_file().path())
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            status: match d.status() {
                git2::Delta::Added => FileStatusKind::New,
                git2::Delta::Deleted => FileStatusKind::Deleted,
                git2::Delta::Renamed => FileStatusKind::Renamed,
                git2::Delta::Typechange => FileStatusKind::TypeChange,
                _ => FileStatusKind::Modified,
            },
        })
        .collect()
}

/// Merge staged gitlink entries into `status.staged`, keeping the list
/// path-sorted and free of duplicates (in case a libgit2 version does
/// report the entry despite `exclude_submodules`).
pub fn append_staged_gitlinks(repo: &git2::Repository, status: &mut WorkingDirStatus) {
    let mut gitlinks = staged_gitlinks(repo);
    if gitlinks.is_empty() {
        return;
    }
    status.staged.append(&mut gitlinks);
    status.staged.sort_by(|a, b| a.path.cmp(&b.path));
    status.staged.dedup_by(|a, b| a.path == b.path);
}

impl GitRepo {
    /// Get working directory status
    pub fn status(&self) -> Result<WorkingDirStatus> {
        if self.is_effectively_bare() {
            return Ok(WorkingDirStatus::default());
        }
        let mut opts = StatusOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .exclude_submodules(true);

        let statuses = self
            .repo
            .statuses(Some(&mut opts))
            .context("Failed to get status")?;

        let mut status = working_dir_status_from_statuses(&statuses);
        append_staged_gitlinks(&self.repo, &mut status);
        Ok(status)
    }

    /// Stage a file.
    ///
    /// Handles all working-tree states: modified files are added to the index,
    /// deleted files are removed from the index, and new (untracked) files are
    /// added normally.
    pub fn stage_file(&self, path: &str) -> Result<()> {
        self.ensure_not_bare()?;
        let mut index = self.repo.index().context("Failed to get index")?;

        // Check if the file exists on disk to determine correct index operation
        let full_path = self.workdir().map(|wd| wd.join(path));
        let exists_on_disk = full_path.as_ref().is_some_and(|p| p.exists());

        if exists_on_disk {
            // File exists: add it (works for new + modified + typechange)
            index
                .add_path(Path::new(path))
                .context("Failed to stage file")?;
        } else {
            // File was deleted from disk: remove from index to stage the deletion
            index
                .remove_path(Path::new(path))
                .context("Failed to stage deleted file")?;
        }
        index.write().context("Failed to write index")?;
        Ok(())
    }

    /// Unstage a file.
    ///
    /// Handles all index states: for files that exist in HEAD, resets the index
    /// entry to the HEAD version. For newly added files (INDEX_NEW) that have
    /// no HEAD version, removes them from the index entirely.
    pub fn unstage_file(&self, path: &str) -> Result<()> {
        self.ensure_not_bare()?;

        // Check file status to determine if this is a newly added file
        let file_status = self
            .repo
            .status_file(Path::new(path))
            .unwrap_or(Status::empty());

        if file_status.contains(Status::INDEX_NEW) {
            // Newly added file: no HEAD version exists, so remove from index
            let mut index = self.repo.index().context("Failed to get index")?;
            index
                .remove_path(Path::new(path))
                .context("Failed to unstage new file")?;
            index.write().context("Failed to write index")?;
        } else {
            // File exists in HEAD: reset index entry to HEAD version
            let head = self.repo.head().context("Failed to get HEAD")?;
            let head_commit = head.peel_to_commit().context("Failed to get HEAD commit")?;
            self.repo
                .reset_default(Some(head_commit.as_object()), [Path::new(path)])
                .context("Failed to unstage file")?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod gitlink_tests {
    use super::super::GitRepo;
    use super::*;
    use std::fs;
    use std::path::PathBuf;
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

    fn temp_dir(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("whisper-git-{name}-{unique}"));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn init_repo(dir: &Path) {
        run_git(dir, &["init", "-b", "main"]);
        run_git(dir, &["config", "user.name", "Test"]);
        run_git(dir, &["config", "user.email", "test@example.com"]);
    }

    fn commit_file(dir: &Path, path: &str, content: &str, msg: &str) {
        fs::write(dir.join(path), content).expect("write file");
        run_git(dir, &["add", path]);
        run_git(dir, &["commit", "-m", msg]);
    }

    /// Regression: `exclude_submodules` hid staged gitlink changes, so
    /// the app's own "Update pointer" flow staged a change that never
    /// appeared in the staging well and commit refused with "No staged
    /// changes". A staged pointer bump must show up as a staged
    /// Modified entry — and only there (not unstaged/untracked).
    #[test]
    fn staged_submodule_pointer_is_visible() {
        let root = temp_dir("gitlink-status");
        let sub_src = root.join("sub");
        fs::create_dir_all(&sub_src).unwrap();
        init_repo(&sub_src);
        commit_file(&sub_src, "lib.txt", "v1\n", "sub v1");

        let parent = root.join("parent");
        fs::create_dir_all(&parent).unwrap();
        init_repo(&parent);
        commit_file(&parent, "readme.txt", "hi\n", "initial");
        run_git(
            &parent,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                sub_src.to_str().unwrap(),
                "sub",
            ],
        );
        run_git(&parent, &["commit", "-m", "add submodule"]);

        let repo = GitRepo::open(&parent).expect("open parent");
        assert_eq!(
            repo.status().expect("status").total_files(),
            0,
            "clean parent must stay clean"
        );

        // Advance the submodule and stage the new pointer, exactly as
        // the in-app flow does (commit inside submodule, then
        // stage_file on the gitlink path from the parent).
        let sub_wt = parent.join("sub");
        commit_file(&sub_wt, "lib.txt", "v2\n", "sub v2");
        repo.stage_file("sub").expect("stage gitlink");

        let status = repo.status().expect("status");
        assert_eq!(
            status
                .staged
                .iter()
                .map(|f| (f.path.as_str(), f.status))
                .collect::<Vec<_>>(),
            vec![("sub", FileStatusKind::Modified)],
            "staged pointer bump must be visible as staged"
        );
        assert!(status.unstaged.is_empty(), "no unstaged entries expected");
        assert!(status.untracked.is_empty(), "no untracked entries expected");

        let _ = fs::remove_dir_all(&root);
    }

    /// A merge conflict on the submodule pointer must be reported as
    /// conflicted ONLY — tree→index diffs include conflict entries as
    /// `Delta::Conflicted` with gitlink modes, which the staged-gitlink
    /// supplement must not misfile as a staged change (the path would
    /// otherwise show in both lists and "has staged changes" gates
    /// would fire mid-conflict).
    #[test]
    fn conflicted_submodule_pointer_is_not_staged() {
        let root = temp_dir("gitlink-conflict");
        let sub_src = root.join("sub");
        fs::create_dir_all(&sub_src).unwrap();
        init_repo(&sub_src);
        // v2 and v3 must be *divergent* — for ancestor/descendant
        // pointers git fast-forwards the gitlink merge instead of
        // conflicting.
        commit_file(&sub_src, "lib.txt", "v1\n", "sub v1");
        let v1 = run_git(&sub_src, &["rev-parse", "HEAD"]);
        run_git(&sub_src, &["checkout", "-b", "side"]);
        commit_file(&sub_src, "lib.txt", "v2\n", "sub v2");
        let v2 = run_git(&sub_src, &["rev-parse", "HEAD"]);
        run_git(&sub_src, &["checkout", "main"]);
        commit_file(&sub_src, "lib.txt", "v3\n", "sub v3");
        let v3 = run_git(&sub_src, &["rev-parse", "HEAD"]);

        let parent = root.join("parent");
        fs::create_dir_all(&parent).unwrap();
        init_repo(&parent);
        commit_file(&parent, "readme.txt", "hi\n", "initial");
        run_git(
            &parent,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                sub_src.to_str().unwrap(),
                "sub",
            ],
        );
        let sub_wt = parent.join("sub");
        run_git(&sub_wt, &["checkout", &v1]);
        run_git(&parent, &["add", "sub"]);
        run_git(&parent, &["commit", "-m", "pin v1"]);

        // Two branches pin the submodule to different commits …
        run_git(&parent, &["checkout", "-b", "b1"]);
        run_git(&sub_wt, &["checkout", &v2]);
        run_git(&parent, &["add", "sub"]);
        run_git(&parent, &["commit", "-m", "pin v2"]);
        run_git(&parent, &["checkout", "main"]);
        run_git(&sub_wt, &["checkout", &v3]);
        run_git(&parent, &["add", "sub"]);
        run_git(&parent, &["commit", "-m", "pin v3"]);

        // … and merging them conflicts on the gitlink (merge exits
        // non-zero; assert the conflict happened rather than success).
        let out = Command::new("git")
            .args(["merge", "b1"])
            .current_dir(&parent)
            .output()
            .expect("run git merge");
        assert!(!out.status.success(), "gitlink merge must conflict");

        let repo = GitRepo::open(&parent).expect("open parent");
        let status = repo.status().expect("status");
        assert_eq!(
            status
                .conflicted
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            vec!["sub"],
            "the pointer conflict must be reported as conflicted"
        );
        assert!(
            !status.staged.iter().any(|f| f.path == "sub"),
            "a conflicted pointer must not be misfiled as staged"
        );

        let _ = fs::remove_dir_all(&root);
    }
}
