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

        Ok(working_dir_status_from_statuses(&statuses))
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
