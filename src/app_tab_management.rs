use super::*;

impl App {
    pub(crate) fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let name = self.tabs[index].0.name.clone();
        self.tabs.remove(index);
        self.active_tab = self.tab_bar.remove_tab(index);
        self.toast_manager
            .push(format!("Closed {}", name), ToastSeverity::Info);
        if self.tabs.is_empty() {
            // Falling back to the welcome surface — make sure it shows the
            // freshest MRU order (the just-closed tab may have been opened
            // since welcome was last populated).
            self.welcome_view.set_recent(&self.config.recent_repos);
        } else {
            self.refresh_status();
        }
    }

    pub(crate) fn switch_tab(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active_tab = index;
            self.tab_bar.set_active(index);
            self.refresh_status();
        }
    }
}
