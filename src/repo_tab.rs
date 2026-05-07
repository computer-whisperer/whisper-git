//! Per-tab repo state: a `GitRepo` plus the cached lists the sidebar
//! displays. Opens synchronously today; async refresh + filesystem
//! watcher get re-wired in Phase 4.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};

use crate::git::{
    BranchTip, GitRepo, StashEntry, SubmoduleInfo, TagInfo, WorktreeInfo,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum SidebarSection {
    Local,
    Remote,
    Tags,
    Submodules,
    Worktrees,
    Stashes,
}

impl SidebarSection {
    pub fn key(self) -> &'static str {
        match self {
            Self::Local => "LOCAL",
            Self::Remote => "REMOTE",
            Self::Tags => "TAGS",
            Self::Submodules => "SUBMODULES",
            Self::Worktrees => "WORKTREES",
            Self::Stashes => "STASHES",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::Remote => "Remote",
            Self::Tags => "Tags",
            Self::Submodules => "Submodules",
            Self::Worktrees => "Worktrees",
            Self::Stashes => "Stashes",
        }
    }

    pub const ALL: [SidebarSection; 6] = [
        Self::Local,
        Self::Remote,
        Self::Tags,
        Self::Submodules,
        Self::Worktrees,
        Self::Stashes,
    ];
}

/// A logical entry in the sidebar — the keyboard cursor lands on these.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum SidebarSelection {
    Local(String),
    Remote { remote: String, branch: String },
    Tag(String),
    Submodule(String),
    Worktree(String),
    Stash(usize),
}

#[derive(Default)]
pub struct SidebarState {
    pub collapsed: HashSet<SidebarSection>,
    pub selected: Option<SidebarSelection>,
}

impl SidebarState {
    pub fn toggle(&mut self, section: SidebarSection) {
        if !self.collapsed.remove(&section) {
            self.collapsed.insert(section);
        }
    }

    pub fn is_collapsed(&self, section: SidebarSection) -> bool {
        self.collapsed.contains(&section)
    }
}

pub struct RepoTab {
    pub repo: GitRepo,
    pub repo_name: String,
    pub current_branch: String,
    pub branch_tips: Vec<BranchTip>,
    pub remotes: Vec<String>,
    pub tags: Vec<TagInfo>,
    pub worktrees: Vec<WorktreeInfo>,
    pub submodules: Vec<SubmoduleInfo>,
    pub stashes: Vec<StashEntry>,
    pub sidebar: SidebarState,
}

impl RepoTab {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let repo = GitRepo::open(&path).context("open repository")?;
        let mut tab = Self {
            repo_name: repo.repo_name(),
            current_branch: String::new(),
            branch_tips: Vec::new(),
            remotes: Vec::new(),
            tags: Vec::new(),
            worktrees: Vec::new(),
            submodules: Vec::new(),
            stashes: Vec::new(),
            sidebar: SidebarState::default(),
            repo,
        };
        tab.refresh();
        Ok(tab)
    }

    /// Re-query everything from the underlying repo. Synchronous; the
    /// async equivalent goes back online in Phase 4.
    pub fn refresh(&mut self) {
        self.current_branch = self.repo.current_branch().unwrap_or_default();
        self.branch_tips = self.repo.branch_tips().unwrap_or_default();
        self.remotes = self.repo.remote_names();
        self.tags = self.repo.tags().unwrap_or_default();
        self.worktrees = self.repo.worktrees().unwrap_or_default();
        self.submodules = self.repo.submodules().unwrap_or_default();
        self.stashes = self.repo.stash_list();
    }

    /// Local branches sorted alphabetically.
    pub fn local_branches(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .branch_tips
            .iter()
            .filter(|t| !t.is_remote)
            .map(|t| t.name.as_str())
            .collect();
        v.sort_unstable();
        v
    }

    /// Remote branches grouped by remote name. Within each remote, the
    /// branch list is sorted alphabetically. `origin/HEAD` and similar
    /// symref aliases are filtered out — git2 surfaces them as branches
    /// but they're not meaningful entries in a sidebar.
    pub fn remote_branches(&self) -> Vec<(String, Vec<String>)> {
        let mut by_remote: std::collections::BTreeMap<String, Vec<String>> =
            Default::default();
        for tip in &self.branch_tips {
            if !tip.is_remote {
                continue;
            }
            let (remote, branch) = match tip.name.split_once('/') {
                Some(p) => p,
                None => continue,
            };
            if branch == "HEAD" {
                continue;
            }
            by_remote
                .entry(remote.to_string())
                .or_default()
                .push(branch.to_string());
        }
        for branches in by_remote.values_mut() {
            branches.sort_unstable();
        }
        by_remote.into_iter().collect()
    }
}
