use super::*;

impl App {
    pub(crate) fn next_wake_deadline(&self, now: Instant) -> Option<Instant> {
        let mut next_wake: Option<Instant> = None;
        let mut merge = |t: Instant| {
            next_wake = Some(next_wake.map_or(t, |w| w.min(t)));
        };

        // Active animations (spinners, button pulse) -> ~60fps.
        if let Some((_, vs)) = self.tabs.get(self.active_tab) {
            let animating = vs.header_bar.fetching
                || vs.header_bar.pulling
                || vs.header_bar.pushing
                || vs.generic_op_receiver.is_some()
                || self.ai_commit_receiver.is_some();
            if animating {
                merge(self.last_frame_time + Duration::from_millis(16));
            }
        }

        // Active toasts (fade animation).
        if self.toast_manager.has_active_toasts() {
            merge(self.last_frame_time + Duration::from_millis(16));
        }

        // Cursor blink.
        if self.has_focused_text_input() {
            merge(self.last_frame_time + Duration::from_millis(530));
        }

        // Status refresh timer.
        merge(self.last_status_refresh + Duration::from_secs(3));

        // Ref reconciliation timer.
        merge(self.last_ref_check + Duration::from_secs(5));

        // Pending resize debounce.
        if let Some(last_resize) = self.resize_debounce {
            merge(last_resize + Duration::from_millis(100));
        }

        // CI status polling timer (all tabs).
        // Continue polling if we already have CI results.
        for (_, vs) in &self.tabs {
            if !vs.ci_receivers.is_empty() || vs.ci_results.is_empty() {
                continue;
            }
            let any_pending = vs
                .ci_results
                .iter()
                .any(|r| r.status.state == ci::CiState::Pending);
            let fast_poll = any_pending
                || vs
                    .last_push_time
                    .is_some_and(|t| now.duration_since(t).as_secs() < 300);
            let interval = if fast_poll { 15 } else { 300 };
            merge(vs.last_ci_fetch + Duration::from_secs(interval));
        }

        next_wake
    }

    /// Check if any text input is currently focused (for cursor blink scheduling).
    pub(crate) fn has_focused_text_input(&self) -> bool {
        let Some((_, vs)) = self.tabs.get(self.active_tab) else {
            return false;
        };
        if vs.staging_well.subject_input.is_focused() {
            return true;
        }
        if vs.staging_well.body_area.is_focused() {
            return true;
        }
        if vs.branch_sidebar.has_text_focus() {
            return true;
        }
        if vs.commit_graph_view.search_bar.is_active() {
            return true;
        }
        if self.branch_name_dialog.is_visible() {
            return true;
        }
        if self.remote_dialog.is_visible() {
            return true;
        }
        if self.repo_dialog.is_visible() {
            return true;
        }
        if self.token_dialog.is_visible() {
            return true;
        }
        false
    }
}
