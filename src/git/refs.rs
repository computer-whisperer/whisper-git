//! Branch, tag, remote, and commit operations on GitRepo.
//!
//! Contains the second logical group of GitRepo methods: checkout, branch CRUD,
//! tag CRUD, remote management, stash listing, commit amend/discard, and
//! detailed commit/submodule queries.

use anyhow::{Context, Result};
use git2::{Oid, Status};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{CommitSubmoduleEntry, FullCommitInfo, GitRepo, StashEntry};

impl GitRepo {
    /// Check if the working directory has any uncommitted changes (staged or unstaged).
    /// Returns the total number of changed files, or 0 for bare repos.
    pub fn uncommitted_change_count(&self) -> usize {
        if self.is_effectively_bare() {
            return 0;
        }
        self.status().map(|s| s.total_files()).unwrap_or(0)
    }

    /// Get a suitable directory for running git CLI commands.
    /// Returns the workdir if available, otherwise falls back to the git dir.
    /// This allows push/fetch/pull to work on bare repos.
    pub fn git_command_dir(&self) -> PathBuf {
        self.workdir()
            .unwrap_or_else(|| self.repo.path())
            .to_path_buf()
    }

    /// Check if git user.name and user.email are configured.
    /// Returns true if `repo.signature()` would succeed (needed for commits).
    pub fn has_user_config(&self) -> bool {
        self.repo.signature().is_ok()
    }

    /// Check if the repo uses Git LFS (has `filter=lfs` in .gitattributes).
    pub fn has_lfs(&self) -> bool {
        // Check workdir .gitattributes first (covers normal repos)
        if let Some(workdir) = self.workdir() {
            let attrs = workdir.join(".gitattributes");
            if let Ok(content) = std::fs::read_to_string(attrs)
                && content.contains("filter=lfs")
            {
                return true;
            }
        }
        // For bare repos, check HEAD tree for .gitattributes
        if let Ok(head) = self.repo.head()
            && let Some(tree) = head.peel_to_tree().ok()
            && let Ok(entry) = tree.get_name(".gitattributes").ok_or(())
            && let Ok(blob) = self.repo.find_blob(entry.id())
            && let Ok(content) = std::str::from_utf8(blob.content())
        {
            return content.contains("filter=lfs");
        }
        false
    }

    /// Check if any remotes are configured.
    pub fn has_remotes(&self) -> bool {
        self.repo.remotes().is_ok_and(|r| !r.is_empty())
    }

    /// Find the default remote name (usually "origin")
    pub fn default_remote(&self) -> Result<String> {
        // Try to find the upstream remote for the current branch
        if let Ok(head) = self.repo.head()
            && let Some(name) = head.shorthand()
            && let Ok(branch) = self.repo.find_branch(name, git2::BranchType::Local)
            && let Ok(upstream) = branch.upstream()
            && let Ok(Some(upstream_name)) = upstream.name()
        {
            // upstream name is like "origin/main", extract remote part
            if let Some(remote) = upstream_name.split('/').next() {
                return Ok(remote.to_string());
            }
        }
        // Fallback: try "origin"
        if self.repo.find_remote("origin").is_ok() {
            return Ok("origin".to_string());
        }
        // Fallback: first remote
        let remotes = self.repo.remotes().context("No remotes configured")?;
        remotes
            .get(0)
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("No remotes configured"))
    }

    /// Checkout a local branch by name
    pub fn checkout_branch(&self, name: &str) -> Result<()> {
        let branch = self
            .repo
            .find_branch(name, git2::BranchType::Local)
            .with_context(|| format!("Branch '{}' not found", name))?;
        let reference = branch
            .get()
            .resolve()
            .context("Failed to resolve branch reference")?;
        let commit = reference
            .peel_to_commit()
            .context("Failed to peel to commit")?;
        let tree = commit.tree().context("Failed to get tree")?;

        self.repo
            .checkout_tree(
                tree.as_object(),
                Some(git2::build::CheckoutBuilder::new().safe()),
            )
            .context("Failed to checkout tree")?;

        let refname = format!("refs/heads/{}", name);
        self.repo
            .set_head(&refname)
            .with_context(|| format!("Failed to set HEAD to {}", name))?;

        Ok(())
    }

    /// Checkout a remote branch, creating a local tracking branch
    pub fn checkout_remote_branch(&self, remote: &str, branch: &str) -> Result<()> {
        // Check if local branch already exists
        if self
            .repo
            .find_branch(branch, git2::BranchType::Local)
            .is_ok()
        {
            // Just checkout the existing local branch
            return self.checkout_branch(branch);
        }

        // Find the remote branch
        let remote_branch_name = format!("{}/{}", remote, branch);
        let remote_ref = self
            .repo
            .find_branch(&remote_branch_name, git2::BranchType::Remote)
            .with_context(|| format!("Remote branch '{}' not found", remote_branch_name))?;
        let commit = remote_ref
            .get()
            .peel_to_commit()
            .context("Failed to peel remote branch to commit")?;

        // Create local tracking branch
        let mut local_branch = self
            .repo
            .branch(branch, &commit, false)
            .with_context(|| format!("Failed to create local branch '{}'", branch))?;

        // Set upstream
        local_branch
            .set_upstream(Some(&remote_branch_name))
            .context("Failed to set upstream")?;

        // Checkout
        let tree = commit.tree().context("Failed to get tree")?;
        self.repo
            .checkout_tree(
                tree.as_object(),
                Some(git2::build::CheckoutBuilder::new().safe()),
            )
            .context("Failed to checkout tree")?;

        let refname = format!("refs/heads/{}", branch);
        self.repo.set_head(&refname)?;

        Ok(())
    }

    /// Move HEAD to point at the given branch without checking out any files.
    /// Useful for bare repos (no working directory) where you just want to update
    /// which branch HEAD references.
    pub fn set_head_to(&self, branch_name: &str) -> Result<()> {
        let refname = format!("refs/heads/{}", branch_name);
        // Verify the branch exists
        self.repo
            .find_branch(branch_name, git2::BranchType::Local)
            .with_context(|| format!("Branch '{}' not found", branch_name))?;
        self.repo
            .set_head(&refname)
            .with_context(|| format!("Failed to set HEAD to '{}'", branch_name))?;
        Ok(())
    }

    /// Delete a local branch (refuses to delete the current branch or one checked out in a worktree)
    pub fn delete_branch(&self, name: &str) -> Result<()> {
        // Check if this branch is the current branch (handle bare-repo failures gracefully)
        if let Ok(current) = self.current_branch()
            && current == name
        {
            anyhow::bail!("Cannot delete the currently checked-out branch");
        }
        let mut branch = self
            .repo
            .find_branch(name, git2::BranchType::Local)
            .with_context(|| format!("Branch '{}' not found", name))?;
        // Check if the branch is checked out in any worktree
        if branch.is_head() {
            anyhow::bail!("Cannot delete the currently checked-out branch");
        }
        branch
            .delete()
            .with_context(|| format!("Failed to delete branch '{}'", name))?;
        Ok(())
    }

    /// Rename a local branch
    pub fn rename_branch(&self, old_name: &str, new_name: &str, force: bool) -> Result<()> {
        let mut branch = self
            .repo
            .find_branch(old_name, git2::BranchType::Local)
            .with_context(|| format!("Branch '{}' not found", old_name))?;
        branch
            .rename(new_name, force)
            .with_context(|| format!("Failed to rename branch '{}' to '{}'", old_name, new_name))?;
        Ok(())
    }

    /// Reset HEAD to a given commit
    pub fn reset_to_commit(&self, oid: Oid, mode: git2::ResetType) -> Result<()> {
        let commit = self
            .repo
            .find_commit(oid)
            .with_context(|| format!("Failed to find commit {}", oid))?;
        self.repo
            .reset(commit.as_object(), mode, None)
            .with_context(|| format!("Failed to reset to {}", oid))?;
        Ok(())
    }

    /// Create a new branch at a given commit OID
    pub fn create_branch_at(&self, name: &str, oid: Oid) -> Result<()> {
        let commit = self
            .repo
            .find_commit(oid)
            .with_context(|| format!("Failed to find commit {}", oid))?;
        self.repo
            .branch(name, &commit, false)
            .with_context(|| format!("Failed to create branch '{}' at {}", name, oid))?;
        Ok(())
    }

    /// Create a lightweight tag at a given commit OID
    pub fn create_tag(&self, name: &str, oid: Oid) -> Result<()> {
        let commit = self
            .repo
            .find_commit(oid)
            .with_context(|| format!("Failed to find commit {}", oid))?;
        self.repo
            .tag_lightweight(name, commit.as_object(), false)
            .with_context(|| format!("Failed to create tag '{}' at {}", name, oid))?;
        Ok(())
    }

    /// Delete a tag by name
    pub fn delete_tag(&self, name: &str) -> Result<()> {
        self.repo
            .tag_delete(name)
            .with_context(|| format!("Failed to delete tag '{}'", name))?;
        Ok(())
    }

    /// List all stash entries using git CLI (avoids &mut self requirement of libgit2)
    pub fn stash_list(&self) -> Vec<StashEntry> {
        let workdir = match self.workdir().or_else(|| Some(self.repo.path())) {
            Some(p) => p,
            None => return Vec::new(),
        };
        let output = match std::process::Command::new("git")
            .args(["stash", "list", "--format=%gd%x00%s%x00%ct"])
            .current_dir(workdir)
            .output()
        {
            Ok(o) if o.status.success() => o,
            _ => return Vec::new(),
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout
            .lines()
            .enumerate()
            .filter_map(|(i, line)| {
                let parts: Vec<&str> = line.splitn(3, '\0').collect();
                if parts.len() >= 2 {
                    let message = parts[1].to_string();
                    let time = parts
                        .get(2)
                        .and_then(|t| t.parse::<i64>().ok())
                        .unwrap_or(0);
                    Some(StashEntry {
                        index: i,
                        message,
                        time,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Amend the last commit with the current index and a new message
    pub fn amend_commit(&self, message: &str) -> Result<Oid> {
        self.ensure_not_bare()?;
        let head = self.repo.head().context("Failed to get HEAD")?;
        let head_commit = head.peel_to_commit().context("Failed to get HEAD commit")?;
        let mut index = self.repo.index().context("Failed to get index")?;
        let tree_oid = index.write_tree().context("Failed to write tree")?;
        let tree = self
            .repo
            .find_tree(tree_oid)
            .context("Failed to find tree")?;

        let oid = head_commit
            .amend(
                Some("HEAD"),
                None, // keep author
                None, // keep committer
                None, // keep encoding
                Some(message),
                Some(&tree),
            )
            .context("Failed to amend commit")?;

        Ok(oid)
    }

    /// Get the HEAD commit's message split into (subject, body)
    pub fn head_commit_message(&self) -> Option<(String, String)> {
        let head = self.repo.head().ok()?;
        let commit = head.peel_to_commit().ok()?;
        let msg = commit.message()?.to_string();
        let mut lines = msg.splitn(2, '\n');
        let subject = lines.next().unwrap_or("").trim().to_string();
        let body = lines.next().unwrap_or("").trim().to_string();
        Some((subject, body))
    }

    /// Discard working directory changes for a file by checking out from HEAD
    /// Discard unstaged changes to a file, restoring it to the index state.
    ///
    /// Handles all working-tree states:
    /// - Modified/deleted: checkout from index (restores content)
    /// - New (untracked): delete the file from disk
    pub fn discard_file(&self, path: &str) -> Result<()> {
        // Check file status to determine correct discard action
        let file_status = self
            .repo
            .status_file(Path::new(path))
            .unwrap_or(Status::empty());

        if file_status.contains(Status::WT_NEW) {
            // Untracked file: delete from disk
            if let Some(workdir) = self.workdir() {
                let full_path = workdir.join(path);
                if full_path.is_dir() {
                    std::fs::remove_dir_all(&full_path)
                        .with_context(|| format!("Failed to remove directory {}", path))?;
                } else {
                    std::fs::remove_file(&full_path)
                        .with_context(|| format!("Failed to remove untracked file {}", path))?;
                }
            }
            Ok(())
        } else {
            // Modified, deleted, typechange: restore from index/HEAD
            let mut checkout_builder = git2::build::CheckoutBuilder::new();
            checkout_builder.path(path).force();
            self.repo
                .checkout_head(Some(&mut checkout_builder))
                .with_context(|| format!("Failed to discard changes in {}", path))?;
            Ok(())
        }
    }

    /// Get full commit information for the detail panel
    pub fn full_commit_info(&self, oid: Oid) -> Result<FullCommitInfo> {
        let commit = self
            .repo
            .find_commit(oid)
            .with_context(|| format!("Failed to find commit {}", oid))?;

        let author = commit.author();

        let parent_short_ids: Vec<String> = commit
            .parent_ids()
            .map(|id| id.to_string().get(..7).unwrap_or("").to_string())
            .collect();

        Ok(FullCommitInfo {
            id: commit.id(),
            short_id: commit.id().to_string().get(..7).unwrap_or("").to_string(),
            full_message: commit.message().unwrap_or("").to_string(),
            author_name: author.name().unwrap_or("Unknown").to_string(),
            author_email: author.email().unwrap_or("").to_string(),
            author_time: author.when().seconds(),
            parent_short_ids,
        })
    }

    /// Get submodule entries pinned by a specific commit's tree.
    ///
    /// Walks the tree for entries with `ObjectType::Commit` (git's representation
    /// of submodule pointers). Parses `.gitmodules` blob from the same tree for
    /// name→path mapping. Compares against parent commit's tree to detect changes.
    pub fn submodules_at_commit(&self, oid: Oid) -> Result<Vec<CommitSubmoduleEntry>> {
        let commit = self
            .repo
            .find_commit(oid)
            .context("Failed to find commit")?;
        let tree = commit.tree().context("Failed to get commit tree")?;

        // Collect parent tree submodule entries for change detection
        let parent_pins: HashMap<String, Oid> = if commit.parent_count() > 0 {
            if let Ok(parent) = commit.parent(0) {
                if let Ok(parent_tree) = parent.tree() {
                    Self::collect_submodule_pins(&parent_tree)
                } else {
                    HashMap::new()
                }
            } else {
                HashMap::new()
            }
        } else {
            HashMap::new()
        };

        // Parse .gitmodules from this tree for name→path mapping
        let name_map = self.parse_gitmodules_from_tree(&tree);

        // Collect submodule entries from this commit's tree
        let pins = Self::collect_submodule_pins(&tree);

        let mut entries = Vec::new();
        for (path, pinned_oid) in &pins {
            let name = name_map.get(path).cloned().unwrap_or_else(|| path.clone());
            let parent_oid = parent_pins.get(path).copied();
            let changed = match parent_oid {
                Some(parent) => parent != *pinned_oid,
                None => true, // new submodule
            };

            entries.push(CommitSubmoduleEntry {
                name,
                path: path.clone(),
                pinned_oid: *pinned_oid,
                changed,
                parent_oid,
            });
        }

        // Sort: changed entries first, then by name
        entries.sort_by(|a, b| b.changed.cmp(&a.changed).then(a.name.cmp(&b.name)));

        Ok(entries)
    }

    /// Collect all submodule pin entries (ObjectType::Commit) from a tree.
    fn collect_submodule_pins(tree: &git2::Tree) -> HashMap<String, Oid> {
        let mut pins = HashMap::new();
        let _ = tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
            if entry.kind() == Some(git2::ObjectType::Commit)
                && let Some(name) = entry.name()
            {
                pins.insert(format!("{}{}", root, name), entry.id());
            }
            git2::TreeWalkResult::Ok
        });
        pins
    }

    /// Parse `.gitmodules` blob from a tree to build a path→name mapping.
    fn parse_gitmodules_from_tree(&self, tree: &git2::Tree) -> HashMap<String, String> {
        let mut map = HashMap::new();

        let Some(entry) = tree.get_name(".gitmodules") else {
            return map;
        };
        let Ok(obj) = entry.to_object(&self.repo) else {
            return map;
        };
        let Ok(blob) = obj.peel_to_blob() else {
            return map;
        };

        // Simple line-based parser for .gitmodules INI format
        let content = String::from_utf8_lossy(blob.content());
        let mut current_name: Option<String> = None;
        let mut current_path: Option<String> = None;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("[submodule ") {
                // Flush previous entry
                if let (Some(path), Some(name)) = (&current_path, &current_name) {
                    map.insert(path.clone(), name.clone());
                }
                // Parse name from [submodule "name"]
                current_name = trimmed
                    .strip_prefix("[submodule \"")
                    .and_then(|s| s.strip_suffix("\"]"))
                    .map(|s| s.to_string());
                current_path = None;
            } else if let Some(value) = trimmed.strip_prefix("path") {
                let value = value.trim().strip_prefix('=').unwrap_or(value).trim();
                current_path = Some(value.to_string());
            }
        }
        // Flush last entry
        if let (Some(path), Some(name)) = (&current_path, &current_name) {
            map.insert(path.clone(), name.clone());
        }

        map
    }

    // ---- Remote management ----

    /// Get the URL of a named remote.
    pub fn remote_url(&self, name: &str) -> Option<String> {
        self.repo
            .find_remote(name)
            .ok()
            .and_then(|r| r.url().map(|u| u.to_string()))
    }

    /// Check whether a remote is missing its fetch refspec.
    pub fn remote_missing_fetch_refspec(&self, name: &str) -> bool {
        self.repo
            .find_remote(name)
            .is_ok_and(|r| r.fetch_refspecs().is_ok_and(|s| s.is_empty()))
    }

    /// Add the default fetch refspec for a remote that's missing one.
    /// This typically happens with bare-cloned repos. Adds:
    ///   `+refs/heads/*:refs/remotes/<name>/*`
    pub fn add_default_fetch_refspec(&self, name: &str) -> Result<()> {
        let refspec = format!("+refs/heads/*:refs/remotes/{}/*", name);
        self.repo
            .remote_add_fetch(name, &refspec)
            .with_context(|| format!("Failed to add fetch refspec for remote '{}'", name))?;
        Ok(())
    }

    /// Add a new remote with the given name and URL.
    pub fn add_remote(&self, name: &str, url: &str) -> Result<()> {
        self.repo
            .remote(name, url)
            .with_context(|| format!("Failed to add remote '{}' with url '{}'", name, url))?;
        Ok(())
    }

    /// Delete a remote by name.
    pub fn delete_remote(&self, name: &str) -> Result<()> {
        self.repo
            .remote_delete(name)
            .with_context(|| format!("Failed to delete remote '{}'", name))?;
        Ok(())
    }

    /// Rename a remote.
    pub fn rename_remote(&self, old_name: &str, new_name: &str) -> Result<()> {
        let problems = self
            .repo
            .remote_rename(old_name, new_name)
            .with_context(|| format!("Failed to rename remote '{}' to '{}'", old_name, new_name))?;
        if !problems.is_empty() {
            let msgs: Vec<&str> = problems.iter().flatten().collect();
            if !msgs.is_empty() {
                eprintln!("Remote rename warnings: {:?}", msgs);
            }
        }
        Ok(())
    }

    /// Change the URL of an existing remote.
    pub fn set_remote_url(&self, name: &str, url: &str) -> Result<()> {
        self.repo
            .remote_set_url(name, url)
            .with_context(|| format!("Failed to set URL for remote '{}'", name))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::GitRepo;
    use git2::Oid;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn run_git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
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

    #[test]
    fn submodules_at_commit_includes_nested_gitlink_paths() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let repo_dir = std::env::temp_dir().join(format!("whisper-git-refs-test-{unique}"));
        fs::create_dir_all(&repo_dir).expect("create temp repo dir");

        run_git(&repo_dir, &["init"]);
        fs::write(repo_dir.join("README.md"), "root\n").expect("write README");
        run_git(&repo_dir, &["add", "README.md"]);
        run_git(
            &repo_dir,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
        );
        let target_oid = run_git(&repo_dir, &["rev-parse", "HEAD"]);

        let gitmodules = r#"[submodule "nested-lib"]
	path = libs/nested
	url = ../nested-lib
"#;
        fs::create_dir_all(repo_dir.join("libs")).expect("create libs dir");
        fs::write(repo_dir.join(".gitmodules"), gitmodules).expect("write .gitmodules");
        run_git(&repo_dir, &["add", ".gitmodules"]);
        run_git(
            &repo_dir,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000",
                &target_oid,
                "libs/nested",
            ],
        );
        run_git(
            &repo_dir,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "add nested gitlink",
            ],
        );

        let repo = GitRepo::open(&repo_dir).expect("open repo");
        let oid = repo.head_oid().expect("head oid");
        let entries = repo
            .submodules_at_commit(oid)
            .expect("submodules_at_commit should succeed");

        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.name, "nested-lib");
        assert_eq!(entry.path, "libs/nested");
        assert_eq!(
            entry.pinned_oid,
            Oid::from_str(&target_oid).expect("parse oid")
        );
        assert!(entry.changed);

        let _ = fs::remove_dir_all(&repo_dir);
    }
}
