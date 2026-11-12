use std::collections::{HashMap, HashSet};

use crate::process::{ProcessSnapshot, command_basename};
use crate::tmux::SessionInfo;

#[derive(Debug, Default, Clone)]
pub struct PaneProcessSnapshot {
    pub ports_by_pane: HashMap<String, Vec<u16>>,
    pub command_by_pane: HashMap<String, String>,
    pub live_agent_panes: HashSet<String>,
}

fn parse_pane_pids(sessions: &[SessionInfo]) -> HashMap<String, u32> {
    let mut out = HashMap::new();
    for session in sessions {
        for window in &session.windows {
            for pane in &window.panes {
                if let Some(pid) = pane.pane_pid {
                    out.insert(pane.pane_id.clone(), pid);
                }
            }
        }
    }
    out
}

fn is_shell_command(basename: &str) -> bool {
    matches!(
        basename,
        "bash" | "sh" | "zsh" | "fish" | "tmux" | "login" | "sudo"
    )
}

fn best_command_for_pane(pane_pid: u32, process_snapshot: &ProcessSnapshot) -> Option<String> {
    let descendants = process_snapshot.descendants(&[pane_pid]);
    let mut leaf_candidates: Vec<(usize, String)> = Vec::new();
    let mut fallback_candidates: Vec<(usize, String)> = Vec::new();

    for pid in descendants {
        let Some(info) = process_snapshot.info_by_pid.get(&pid) else {
            continue;
        };
        let basename = command_basename(&info.comm);
        if basename.is_empty() || is_shell_command(basename) {
            continue;
        }
        let candidate = if info.args.is_empty() {
            info.comm.clone()
        } else {
            info.args.trim().to_string()
        };
        let len = candidate.len();
        let is_leaf = process_snapshot
            .children_of
            .get(&pid)
            .is_none_or(|children| children.is_empty());
        if is_leaf {
            leaf_candidates.push((len, candidate));
        } else {
            fallback_candidates.push((len, candidate));
        }
    }

    leaf_candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    if let Some((_, command)) = leaf_candidates.into_iter().next() {
        return Some(command);
    }

    fallback_candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    fallback_candidates
        .into_iter()
        .next()
        .map(|(_, command)| command)
}

/// Scan per-pane process state for the provided sessions.
/// The lookup starts from each pane's PID and walks the process tree, so it can
/// pick up child dev servers spawned by an agent shell and detect when the
/// agent process itself has exited.
pub(crate) fn scan_session_process_snapshot(
    sessions: &[SessionInfo],
    process_snapshot: Option<&ProcessSnapshot>,
) -> Option<PaneProcessSnapshot> {
    let pane_pids = parse_pane_pids(sessions);
    if pane_pids.is_empty() {
        return None;
    }

    let owned_snapshot;
    let process_snapshot = match process_snapshot {
        Some(snapshot) => snapshot,
        None => {
            owned_snapshot = ProcessSnapshot::scan()?;
            &owned_snapshot
        }
    };

    let mut live_agent_panes: HashSet<String> = HashSet::new();
    let mut command_by_pane: HashMap<String, String> = HashMap::new();
    for session in sessions {
        for window in &session.windows {
            for pane in &window.panes {
                let Some(&pane_pid) = pane_pids.get(&pane.pane_id) else {
                    continue;
                };
                if process_snapshot.tree_has_agent(&[pane_pid], &pane.agent) {
                    live_agent_panes.insert(pane.pane_id.clone());
                }
                if let Some(command) = best_command_for_pane(pane_pid, process_snapshot) {
                    command_by_pane.insert(pane.pane_id.clone(), command);
                }
            }
        }
    }

    Some(PaneProcessSnapshot {
        // Fork-local: no listening-port scan (no `lsof`). Every Claude Code
        // instance listens on a random localhost port, so the sidebar only
        // showed noise; the owner does not track ports.
        ports_by_pane: HashMap::new(),
        command_by_pane,
        live_agent_panes,
    })
}

/// Scan listening TCP ports for panes in the provided sessions.
/// The lookup starts from each pane's PID and walks the process tree, so it can
/// pick up child dev servers spawned by an agent shell.
pub fn scan_session_ports(sessions: &[SessionInfo]) -> HashMap<String, Vec<u16>> {
    scan_session_process_snapshot(sessions, None)
        .map(|snapshot| snapshot.ports_by_pane)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fork-local: the sidebar shows no listening ports. Every Claude Code
    /// instance listens on a random localhost port, so the row only showed
    /// noise. A pane whose own process listens must still report none.
    #[test]
    fn scan_reports_no_ports_even_when_pane_process_listens() {
        use crate::tmux::{
            AgentType, AgentWindow, PaneInfo, PaneStatus, PermissionMode, WindowInfo,
            WorktreeMetadata,
        };

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let pid = std::process::id();
        let pane = PaneInfo {
            pane_id: "%1".into(),
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
            pane_pid: Some(pid),
            worktree: WorktreeMetadata::default(),
            session_id: None,
            session_name: String::new(),
            custom_name: String::new(),
            car_emoji: String::new(),
            agent_window: AgentWindow::None,
            ordinal: 0,
            sidebar_spawned: false,
            bg_shell_cmd: None,
        };
        let sessions = vec![SessionInfo {
            session_name: "s".into(),
            windows: vec![WindowInfo {
                window_id: "@1".into(),
                window_name: "w".into(),
                window_active: true,
                auto_rename: false,
                panes: vec![pane],
            }],
        }];
        let snapshot = ProcessSnapshot::from_ps_output(&format!("{pid} 1 claude claude\n"));

        let scanned = scan_session_process_snapshot(&sessions, Some(&snapshot)).unwrap();

        assert!(scanned.ports_by_pane.is_empty());
        assert!(scanned.live_agent_panes.contains("%1"));
        drop(listener);
    }

    #[test]
    fn best_command_for_pane_prefers_leaf_non_shell_command() {
        let snapshot = ProcessSnapshot::from_ps_output(
            "10 1 zsh zsh\n11 10 node /usr/bin/node /tmp/server.js --port 3000\n12 10 git /usr/bin/git status\n",
        );

        let command = best_command_for_pane(10, &snapshot).unwrap();
        assert_eq!(command, "/usr/bin/node /tmp/server.js --port 3000");
    }
}
