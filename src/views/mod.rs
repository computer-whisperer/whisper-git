mod branch_sidebar;
mod commit_detail;
mod commit_graph;
mod diff_view;
mod staging_well;

// Deprecated stub kept for main.rs compatibility. Remove once main.rs is cleaned up.
mod secondary_repos;

pub use branch_sidebar::{BranchSidebar, SidebarAction};
pub use commit_detail::{CommitDetailView, CommitDetailAction};
pub use commit_graph::{CommitGraphView, GraphAction};
pub use diff_view::{DiffView, DiffAction};
pub use staging_well::{StagingWell, StagingAction};

// Deprecated: will be removed once main.rs no longer references SecondaryReposView.
pub use secondary_repos::SecondaryReposView;
