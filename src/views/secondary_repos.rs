//! Deprecated stub - SecondaryReposView was removed in Sprint 16 (data moved to sidebar).
//! This minimal stub exists only because main.rs still references it.
//! TODO: Remove this file and its mod.rs entry once main.rs references are cleaned up.

use crate::git::{SubmoduleInfo, WorktreeInfo};

#[allow(dead_code)]
pub struct SecondaryReposView;

#[allow(dead_code)]
impl SecondaryReposView {
    pub fn new() -> Self {
        Self
    }

    pub fn set_submodules(&mut self, _submodules: Vec<SubmoduleInfo>) {}

    pub fn set_worktrees(&mut self, _worktrees: Vec<WorktreeInfo>) {}
}

impl Default for SecondaryReposView {
    fn default() -> Self {
        Self::new()
    }
}
