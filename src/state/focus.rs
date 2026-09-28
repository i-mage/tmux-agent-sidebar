use super::AppState;
use crate::tmux;

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Focus {
    Filter,
    #[default]
    Panes,
    ActivityLog,
}

#[derive(Debug, Clone)]
pub struct FocusState {
    pub sidebar_focused: bool,
    pub focus: Focus,
    pub focused_pane_id: Option<String>,
    pub prev_focused_pane_id: Option<String>,
    /// Fork-local: pane a click/Enter asked to jump to. The event loop
    /// paints the new highlight first and only then runs the tmux jump,
    /// so the row lights up on the next frame instead of after a chain
    /// of blocking tmux subprocesses.
    pub pending_activation: Option<String>,
}

impl FocusState {
    pub fn new() -> Self {
        Self {
            sidebar_focused: false,
            focus: Focus::Panes,
            focused_pane_id: None,
            prev_focused_pane_id: None,
            pending_activation: None,
        }
    }
}

impl Default for FocusState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn pane_by_id(&self, pane_id: &str) -> Option<&tmux::PaneInfo> {
        for group in &self.repo_groups {
            for (pane, _) in &group.panes {
                if pane.pane_id == pane_id {
                    return Some(pane);
                }
            }
        }
        None
    }

    pub fn selected_pane(&self) -> Option<&tmux::PaneInfo> {
        let target = self
            .layout
            .pane_row_targets
            .get(self.global.selected_pane_row)?;
        self.pane_by_id(&target.pane_id)
    }

    pub fn find_focused_pane(&mut self) {
        // Query tmux directly for the active pane, not through `repo_groups`
        // which only contains agent panes. This allows activity/git info to
        // be displayed even when the focused pane has no agent running.
        // When the sidebar has focus, find_active_pane returns None — preserve
        // the previously focused pane so bottom panel data stays stable.
        if let Some((id, _)) = tmux::find_active_pane(&self.tmux_pane) {
            self.focus_state.focused_pane_id = Some(id);
        }
    }

    /// Move agent selection. Returns true if moved, false if at boundary.
    pub fn move_pane_selection(&mut self, delta: isize) -> bool {
        if self.layout.pane_row_targets.is_empty() {
            return false;
        }
        let len = self.layout.pane_row_targets.len() as isize;
        let next = self.global.selected_pane_row as isize + delta;
        if next >= 0 && next < len {
            self.global.selected_pane_row = next as usize;
            true
        } else {
            false
        }
    }

    pub fn activate_selected_pane(&mut self) {
        if let Some(target_pane_id) = self
            .layout
            .pane_row_targets
            .get(self.global.selected_pane_row)
            .map(|target| target.pane_id.clone())
        {
            // Update the sidebar immediately so the active marker and
            // repo header highlight move without waiting for the next
            // periodic tmux refresh. The tmux side runs in
            // `run_pending_activation`, after the event loop has painted.
            self.focus_state.focused_pane_id = Some(target_pane_id.clone());
            self.focus_state.pending_activation = Some(target_pane_id);
            // The click that got us here gave tmux focus to the sidebar
            // itself, which would paint the cursor's selection colour over
            // the active block until the jump lands. Focus is leaving the
            // sidebar anyway, so paint the final state right away.
            self.focus_state.sidebar_focused = false;
        }
    }

    /// Perform the tmux jump queued by [`Self::activate_selected_pane`].
    /// Returns true when a jump ran.
    pub fn run_pending_activation(&mut self) -> bool {
        let Some(target_pane_id) = self.focus_state.pending_activation.take() else {
            return false;
        };
        // Fork-local: one query for everything the jump needs.
        let info = tmux::display_message(
            &target_pane_id,
            "#{window_id}\t#{pane_current_path}",
        );
        let (window_id, pane_path) = info.split_once('\t').unwrap_or((info.as_str(), ""));
        let (window_id, pane_path) = (window_id.to_string(), pane_path.to_string());
        let same_window = self.pane_in_own_window(&window_id);

        // Make the target the active pane of its window first. select-pane
        // does not switch windows, so a cross-window jump can wake the
        // sidebar there and let it repaint its highlight while still
        // hidden; switching afterwards shows the right row immediately
        // instead of the stale one for a few frames.
        let _ = tmux::run_tmux(&["select-pane", "-t", &target_pane_id]);
        if !same_window && !window_id.is_empty() {
            // The click left tmux focus on this sidebar. Hand it back to the
            // pane that had it, or coming back to this window later sends
            // keystrokes into the sidebar, where letters flip the global
            // status filter.
            let _ = tmux::run_tmux(&["last-pane", "-t", &self.tmux_pane]);
            if signal_window_sidebars(&window_id) {
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            let _ = tmux::run_tmux(&[
                "switch-client",
                "-t",
                &target_pane_id,
                ";",
                "select-window",
                "-t",
                &target_pane_id,
            ]);
        }

        // The jump may land on a window that has no sidebar of its own
        // (e.g. with @sidebar_auto_create off). Summon one there so the
        // sidebar "follows" the jump instead of disappearing. create-only
        // is a no-op when that window already has a sidebar.
        if !window_id.is_empty() {
            let pane_path = if pane_path.is_empty() {
                "~".to_string()
            } else {
                pane_path
            };
            crate::cli::toggle::cmd_toggle(&["--create-only".to_string(), window_id, pane_path]);
        }
        true
    }

    fn pane_in_own_window(&self, window_id: &str) -> bool {
        tmux::display_message(&self.tmux_pane, "#{window_id}") == window_id
    }
}

/// Send SIGUSR1 to every sidebar in `window_id`. Returns true when at
/// least one was signalled, so the caller knows a repaint is coming.
fn signal_window_sidebars(window_id: &str) -> bool {
    let pids = tmux::run_tmux(&["list-panes", "-t", window_id, "-F", "#{@sidebar_pid}"])
        .unwrap_or_default();
    let mut any = false;
    for pid in pids.lines().filter_map(|l| l.trim().parse::<i32>().ok()) {
        if pid > 0 && unsafe { libc::kill(pid, libc::SIGUSR1) } == 0 {
            any = true;
        }
    }
    any
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_default_is_panes() {
        assert_eq!(Focus::default(), Focus::Panes);
    }

    #[test]
    fn focus_state_new_has_expected_initial_values() {
        let state = FocusState::new();
        assert!(!state.sidebar_focused);
        assert_eq!(state.focus, Focus::Panes);
        assert!(state.focused_pane_id.is_none());
        assert!(state.prev_focused_pane_id.is_none());
    }

    #[test]
    fn focus_state_default_delegates_to_new() {
        let state = FocusState::default();
        assert!(!state.sidebar_focused);
        assert_eq!(state.focus, Focus::Panes);
        assert!(state.focused_pane_id.is_none());
        assert!(state.prev_focused_pane_id.is_none());
    }

    // ─── AppState focus accessors ────────────────────────────────────

    use crate::group::{PaneGitInfo, RepoGroup};
    use crate::state::layout::RowTarget;
    use crate::tmux::{AgentType, PaneInfo, PaneStatus, PermissionMode, WorktreeMetadata};

    fn test_pane(id: &str) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            pane_active: false,
            status: PaneStatus::Running,
            attention: false,
            agent: AgentType::Claude,
            path: "/tmp".into(),
            current_command: String::new(),
            prompt: String::new(),
            prompt_is_response: false,
            started_at: None,
            wait_reason: String::new(),
            permission_mode: PermissionMode::Default,
            subagents: vec![],
            pane_pid: None,
            worktree: WorktreeMetadata::default(),
            session_id: None,
            session_name: String::new(),
            custom_name: String::new(),
            ordinal: 0,
            sidebar_spawned: false,
            bg_shell_cmd: None,
        }
    }

    #[test]
    fn pane_by_id_searches_all_groups() {
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![
            RepoGroup {
                name: "alpha".into(),
                has_focus: true,
                panes: vec![(test_pane("%1"), PaneGitInfo::default())],
            },
            RepoGroup {
                name: "beta".into(),
                has_focus: false,
                panes: vec![(test_pane("%42"), PaneGitInfo::default())],
            },
        ];

        assert!(state.pane_by_id("%1").is_some());
        assert!(state.pane_by_id("%42").is_some());
        assert!(state.pane_by_id("%does-not-exist").is_none());
    }

    #[test]
    fn selected_pane_tracks_pane_row_targets() {
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![RepoGroup {
            name: "alpha".into(),
            has_focus: true,
            panes: vec![
                (test_pane("%1"), PaneGitInfo::default()),
                (test_pane("%2"), PaneGitInfo::default()),
            ],
        }];
        state.layout.pane_row_targets = vec![
            RowTarget {
                pane_id: "%1".into(),
            },
            RowTarget {
                pane_id: "%2".into(),
            },
        ];

        state.global.selected_pane_row = 0;
        assert_eq!(
            state.selected_pane().map(|p| p.pane_id.as_str()),
            Some("%1")
        );
        state.global.selected_pane_row = 1;
        assert_eq!(
            state.selected_pane().map(|p| p.pane_id.as_str()),
            Some("%2")
        );
        // Out-of-range cursor → None (not a panic).
        state.global.selected_pane_row = 99;
        assert!(state.selected_pane().is_none());
    }

    #[test]
    fn move_pane_selection_stays_within_bounds() {
        let mut state = AppState::new("%99".into());
        state.layout.pane_row_targets = vec![
            RowTarget {
                pane_id: "%1".into(),
            },
            RowTarget {
                pane_id: "%2".into(),
            },
        ];
        state.global.selected_pane_row = 0;
        assert!(!state.move_pane_selection(-1));
        assert!(state.move_pane_selection(1));
        assert_eq!(state.global.selected_pane_row, 1);
        assert!(!state.move_pane_selection(5));
    }
}
