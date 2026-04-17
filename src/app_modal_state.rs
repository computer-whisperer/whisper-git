use super::*;

impl App {
    /// Set the active modal, hiding the previous one.
    pub(crate) fn set_active_modal(&mut self, modal: ActiveModal) {
        if let Some(prev) = self.active_modal.take() {
            self.set_modal_visible(prev, false);
        }
        self.set_modal_visible(modal, true);
        self.active_modal = Some(modal);
    }

    /// Close the active modal.
    pub(crate) fn close_active_modal(&mut self) {
        if let Some(modal) = self.active_modal.take() {
            self.set_modal_visible(modal, false);
        }
    }

    /// Open an interrupt modal (Error/Confirm) that restores the previous
    /// modal when closed.
    pub(crate) fn open_interrupt_modal(&mut self, modal: ActiveModal) {
        self.interrupted_modal = self.active_modal.take();
        if let Some(prev) = self.interrupted_modal {
            self.set_modal_visible(prev, false);
        }
        self.set_modal_visible(modal, true);
        self.active_modal = Some(modal);
    }

    /// Close an interrupt modal and restore whatever was active before.
    pub(crate) fn close_interrupt_modal(&mut self) {
        if let Some(modal) = self.active_modal.take() {
            self.set_modal_visible(modal, false);
        }
        if let Some(prev) = self.interrupted_modal.take() {
            self.set_modal_visible(prev, true);
            self.active_modal = Some(prev);
        }
    }

    /// Set the visible flag on a dialog by modal variant.
    pub(crate) fn set_modal_visible(&mut self, modal: ActiveModal, visible: bool) {
        match modal {
            ActiveModal::Settings => {
                if visible {
                    // show() is idempotent — just sets visible=true
                    self.settings_dialog.show();
                } else {
                    self.settings_dialog.hide();
                }
            }
            ActiveModal::TokenManager => {
                if !visible {
                    self.token_dialog.hide();
                }
                // show() requires params — handled by open_token_dialog()
            }
            ActiveModal::Confirm => {
                if !visible {
                    self.confirm_dialog.hide();
                }
            }
            ActiveModal::Error => {
                if !visible {
                    self.error_dialog.hide();
                }
            }
            ActiveModal::BranchName => {
                if !visible {
                    self.branch_name_dialog.hide();
                }
            }
            ActiveModal::Remote => {
                if !visible {
                    self.remote_dialog.hide();
                }
            }
            ActiveModal::Pull => {
                if !visible {
                    self.pull_dialog.hide();
                }
            }
            ActiveModal::Push => {
                if !visible {
                    self.push_dialog.hide();
                }
            }
            ActiveModal::Merge => {
                if !visible {
                    self.merge_dialog.hide();
                }
            }
            ActiveModal::Rebase => {
                if !visible {
                    self.rebase_dialog.hide();
                }
            }
            ActiveModal::RepoDialog => {
                if !visible {
                    self.repo_dialog.hide();
                }
            }
            ActiveModal::CloneDialog => {
                if !visible {
                    self.clone_dialog.hide();
                }
            }
        }
    }
}
