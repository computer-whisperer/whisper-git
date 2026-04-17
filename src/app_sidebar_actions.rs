use super::*;

impl App {
    /// Handle a sidebar action by dispatching to the appropriate pending
    /// message or dialog.
    pub(crate) fn handle_sidebar_action(&mut self, action: SidebarAction) {
        let Some((repo_tab, view_state)) = self.tabs.get_mut(self.active_tab) else {
            return;
        };

        match action {
            SidebarAction::Checkout(name) => {
                view_state
                    .pending_messages
                    .push(AppMessage::CheckoutBranch(name));
            }
            SidebarAction::CheckoutRemote(remote, branch) => {
                view_state
                    .pending_messages
                    .push(AppMessage::CheckoutRemoteBranch(remote, branch));
            }
            SidebarAction::Delete(name) => {
                self.confirm_dialog
                    .show("Delete Branch", &format!("Delete local branch '{}'?", name));
                self.pending_confirm_action = Some(AppMessage::DeleteBranch(name));
                self.open_interrupt_modal(ActiveModal::Confirm);
            }
            SidebarAction::ApplyStash(index) => {
                view_state
                    .pending_messages
                    .push(AppMessage::StashApply(index));
            }
            SidebarAction::DropStash(index) => {
                self.confirm_dialog.show(
                    "Drop Stash",
                    &format!("Drop stash@{{{}}}? This cannot be undone.", index),
                );
                self.pending_confirm_action = Some(AppMessage::StashDrop(index));
                self.open_interrupt_modal(ActiveModal::Confirm);
            }
            SidebarAction::DeleteTag(name) => {
                self.confirm_dialog
                    .show("Delete Tag", &format!("Delete tag '{}'?", name));
                self.pending_confirm_action = Some(AppMessage::DeleteTag(name));
                self.open_interrupt_modal(ActiveModal::Confirm);
            }
            SidebarAction::SwitchWorktree(wt_name) => {
                view_state.switch_to_worktree_by_name(&wt_name, &repo_tab.repo);
            }
            SidebarAction::JumpToRef(ref_name) => {
                // Look up OID from branch tips or tags
                let oid = view_state
                    .commit_graph_view
                    .branch_tips
                    .iter()
                    .find(|t| t.name == ref_name)
                    .map(|t| t.oid)
                    .or_else(|| {
                        view_state
                            .commit_graph_view
                            .tags
                            .iter()
                            .find(|t| t.name == ref_name)
                            .map(|t| t.oid)
                    });
                if let Some(oid) = oid {
                    view_state
                        .pending_messages
                        .push(AppMessage::JumpToCommit(oid));
                }
            }
        }
    }
}
