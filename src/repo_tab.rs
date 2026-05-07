//! Per-tab repo state: a `GitRepo` plus the cached lists the sidebar
//! displays. Opens synchronously today; async refresh + filesystem
//! watcher get re-wired in Phase 4.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};

use crate::commit_graph::GraphLayout;
use crate::git::{
    BranchTip, CommitInfo, DiffFile, FullCommitInfo, GitRepo, StashEntry, SubmoduleInfo, TagInfo,
    WorkingDirStatus, WorktreeInfo,
};

/// Cached detail for the currently selected commit. Loaded once per
/// selection change so the History details pane doesn't hit libgit2 on
/// every frame.
pub struct CommitDetail {
    pub info: FullCommitInfo,
    pub files: Vec<DiffFile>,
}

/// Which center-pane view the tab is currently showing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum RepoView {
    #[default]
    Working,
    History,
}

/// Cap for `commit_graph()` — first cut, no infinite-scroll. Plenty for
/// the visible viewport even on big repos. Lifted later if needed.
const COMMIT_LIMIT: usize = 1000;

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
    /// Working directory status (staged / unstaged / untracked / conflicted).
    pub status: WorkingDirStatus,
    /// Controlled commit-message subject. App owns the value; aetna's
    /// `text_input::apply_event` mutates this through `on_event`.
    pub commit_subject: String,
    /// Controlled commit-message body.
    pub commit_body: String,
    /// Currently previewed file (None = no diff selected).
    pub selected_diff_file: Option<String>,
    /// Center-pane view mode (Working diff vs History graph).
    pub view_mode: RepoView,
    /// Reachable commit history, refreshed alongside status. Capped at
    /// `COMMIT_LIMIT` until infinite-scroll comes back.
    pub commits: Vec<CommitInfo>,
    /// Lane / color assignment for `commits`. Rebuilt each refresh.
    pub graph_layout: GraphLayout,
    /// Currently selected commit (drives the right-pane preview when
    /// the History view is active).
    pub selected_commit: Option<git2::Oid>,
    /// Cached detail for `selected_commit`, refreshed via
    /// [`Self::select_commit`] when the selection changes or
    /// invalidated alongside `refresh()` when the commit disappears.
    pub commit_detail: Option<CommitDetail>,
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
            status: WorkingDirStatus::default(),
            commit_subject: String::new(),
            commit_body: String::new(),
            selected_diff_file: None,
            view_mode: RepoView::default(),
            commits: Vec::new(),
            graph_layout: GraphLayout::new(),
            selected_commit: None,
            commit_detail: None,
            repo,
        };
        tab.refresh();
        Ok(tab)
    }

    /// Re-query everything from the underlying repo. Synchronous; the
    /// async equivalent comes back when async polling is re-enabled.
    pub fn refresh(&mut self) {
        self.current_branch = self.repo.current_branch().unwrap_or_default();
        self.branch_tips = self.repo.branch_tips().unwrap_or_default();
        self.remotes = self.repo.remote_names();
        self.tags = self.repo.tags().unwrap_or_default();
        self.worktrees = self.repo.worktrees().unwrap_or_default();
        self.submodules = self.repo.submodules().unwrap_or_default();
        self.stashes = self.repo.stash_list();
        self.status = self.repo.status().unwrap_or_default();
        self.commits = self.repo.commit_graph(COMMIT_LIMIT).unwrap_or_default();
        self.graph_layout.build(&self.commits);
        if let Some(oid) = self.selected_commit
            && !self.commits.iter().any(|c| c.id == oid)
        {
            self.selected_commit = None;
            self.commit_detail = None;
        } else if let Some(oid) = self.selected_commit {
            self.load_commit_detail(oid);
        }
    }

    /// Switch the History view's selected commit. Clears the cached
    /// detail when `oid` is `None`; otherwise loads metadata + diff.
    pub fn select_commit(&mut self, oid: Option<git2::Oid>) {
        if self.selected_commit == oid && self.commit_detail.is_some() {
            return;
        }
        self.selected_commit = oid;
        match oid {
            Some(o) => self.load_commit_detail(o),
            None => self.commit_detail = None,
        }
    }

    fn load_commit_detail(&mut self, oid: git2::Oid) {
        let info = match self.repo.full_commit_info(oid) {
            Ok(i) => i,
            Err(_) => {
                self.commit_detail = None;
                return;
            }
        };
        let files = self.repo.diff_for_commit(oid).unwrap_or_default();
        self.commit_detail = Some(CommitDetail { info, files });
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
        let mut by_remote: std::collections::BTreeMap<String, Vec<String>> = Default::default();
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
