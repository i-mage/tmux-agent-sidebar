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
        // Fork-local: the whole jump is ONE tmux invocation. It used to be
        // five client processes in a row (two queries, select-pane,
        // last-pane, list-panes) before the window switched, which made a
        // sidebar click visibly slower than a click on the status line.
        let args = jump_args(&target_pane_id, &self.tmux_pane);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let _ = tmux::run_tmux(&args);

        // The jump may land on a window that has no sidebar of its own
        // (e.g. with @sidebar_auto_create off). Summon one there so the
        // sidebar "follows" the jump instead of disappearing. create-only
        // is a no-op when that window already has a sidebar. This runs
        // after the switch, so its query costs nothing the eye can see.
        let info = tmux::display_message(&target_pane_id, "#{window_id}\t#{pane_current_path}");
        let (window_id, pane_path) = info.split_once('\t').unwrap_or((info.as_str(), ""));
        if !window_id.is_empty() {
            let pane_path = if pane_path.is_empty() { "~" } else { pane_path };
            crate::cli::toggle::cmd_toggle(&[
                "--create-only".to_string(),
                window_id.to_string(),
                pane_path.to_string(),
            ]);
        }
        true
    }
}

/// Arguments of the single `tmux` invocation that jumps from the sidebar
/// pane `sidebar` to `target`. The server does every step itself:
///
/// 1. `select-pane` makes the target the active pane of its window. It does
///    not switch windows.
/// 2. Only for a target in another window: SIGUSR1 to the sidebars of that
///    window and a 30 ms wait, so they repaint their highlight while still
///    hidden and the switch shows the right row instead of the stale one.
/// 3. `select-window` shows the target window.
/// 4. Only for another window: the click left tmux focus on this sidebar.
///    `last-pane` hands it back to the pane that had it, or coming back to
///    this window later sends keystrokes into the sidebar, where letters
///    flip the global status filter.
/// 5. `switch-client` last: it only matters for a target in another
///    session, and when it fails nothing after it is skipped.
///
/// "Another window" is decided by the server from the sidebar's own window:
/// the list of its pane ids either contains the target or it does not.
pub(crate) fn jump_args(target: &str, sidebar: &str) -> Vec<String> {
    let cross = format!("#{{?#{{m:*{target} *,#{{P:#{{pane_id}} }}}},0,1}}");
    let wake = format!(
        "run-shell -t {target} 'kill -USR1 #{{P:#{{@sidebar_pid}} }} 2>/dev/null; true' ; run-shell -d 0.03"
    );
    let unfocus = format!("#{{&&:#{{pane_active}},{cross}}}");
    let last = format!("last-pane -t {sidebar}");
    [
        "select-pane",
        "-t",
        target,
        ";",
        "if-shell",
        "-F",
        "-t",
        sidebar,
        &cross,
        &wake,
        ";",
        "select-window",
        "-t",
        target,
        ";",
        "if-shell",
        "-F",
        "-t",
        sidebar,
        &unfocus,
        &last,
        ";",
        "switch-client",
        "-t",
        target,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
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
            car_emoji: String::new(),
            agent_window: crate::tmux::AgentWindow::None,
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

    // ─── jump_args: the one-call jump, on a private tmux server ──────

    /// A throwaway tmux server on its own socket (inside the private
    /// TMUX_TMPDIR that `.cargo/config.toml` forces), killed on drop.
    struct JumpServer(String);

    impl JumpServer {
        fn start(name: &str) -> Option<Self> {
            let server = Self(format!("jump-{name}-{}", std::process::id()));
            let out = std::process::Command::new("tmux")
                .args(["-L", &server.0, "-f", "/dev/null"])
                .args(["new-session", "-d", "-s", "a", "-x", "120", "-y", "30"])
                .output()
                .ok()?; // no tmux binary: nothing to test against
            assert!(out.status.success(), "private tmux server did not start");
            Some(server)
        }

        fn run(&self, args: &[&str]) -> String {
            let out = std::process::Command::new("tmux")
                .args(["-L", &self.0, "-f", "/dev/null"])
                .args(args)
                .output()
                .expect("tmux runs");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        fn jump(&self, target: &str, sidebar: &str) {
            let args = jump_args(target, sidebar);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            self.run(&args);
        }

        /// `window_active pane_active` of `pane`, e.g. "1 1".
        fn flags(&self, pane: &str) -> String {
            self.run(&[
                "display-message",
                "-t",
                pane,
                "-p",
                "#{window_active} #{pane_active}",
            ])
        }
    }

    impl Drop for JumpServer {
        fn drop(&mut self) {
            self.run(&["kill-server"]);
        }
    }

    /// Window 0: sidebar (focused, as after a click) + a work pane.
    /// Window 1: the target + a second pane that is the active one.
    fn jump_layout(server: &JumpServer) -> (String, String, String, String) {
        let sidebar = server.run(&["display-message", "-p", "#{pane_id}"]);
        let work = server.run(&["split-window", "-h", "-P", "-F", "#{pane_id}"]);
        server.run(&["select-pane", "-t", &sidebar]);
        let target = server.run(&["new-window", "-d", "-P", "-F", "#{pane_id}"]);
        let other = server.run(&[
            "split-window",
            "-d",
            "-t",
            &target,
            "-P",
            "-F",
            "#{pane_id}",
        ]);
        server.run(&["select-pane", "-t", &other]);
        (sidebar, work, target, other)
    }

    #[test]
    fn jump_args_is_a_single_tmux_invocation_that_switches_window_and_pane() {
        let Some(server) = JumpServer::start("cross") else {
            return;
        };
        let (sidebar, work, target, other) = jump_layout(&server);
        // Stand-in for the sidebar process of the target window.
        let mut waker = std::process::Command::new("sleep")
            .arg("300")
            .spawn()
            .unwrap();
        server.run(&[
            "set-option",
            "-p",
            "-t",
            &other,
            "@sidebar_pid",
            &waker.id().to_string(),
        ]);
        assert_eq!(
            server.flags(&target),
            "0 0",
            "target starts hidden and inactive"
        );

        server.jump(&target, &sidebar);

        assert_eq!(
            server.flags(&target),
            "1 1",
            "target window shown, target pane active"
        );
        assert_eq!(
            server.flags(&sidebar),
            "0 0",
            "focus left the clicked sidebar"
        );
        assert_eq!(
            server.flags(&work),
            "0 1",
            "focus went back to the pane that had it"
        );
        // SIGUSR1 terminates `sleep`, so an exited child proves the signal.
        let mut signalled = false;
        for _ in 0..100 {
            if waker.try_wait().unwrap().is_some() {
                signalled = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = waker.kill();
        assert!(signalled, "the sidebar of the target window got SIGUSR1");
    }

    #[test]
    fn jump_args_inside_the_same_window_only_moves_the_pane() {
        let Some(server) = JumpServer::start("same") else {
            return;
        };
        let (sidebar, work, target, _other) = jump_layout(&server);

        server.jump(&work, &sidebar);

        assert_eq!(
            server.flags(&work),
            "1 1",
            "work pane active, window unchanged"
        );
        assert_eq!(server.flags(&sidebar), "1 0");
        assert_eq!(
            server.flags(&target),
            "0 0",
            "the other window was left alone"
        );
    }
}
