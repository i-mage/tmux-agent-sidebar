#[allow(dead_code, unused_imports)]
mod test_helpers;

use test_helpers::*;
use tmux_agent_sidebar::state::Focus;
use tmux_agent_sidebar::tmux::{AgentType, PaneStatus, SessionInfo, WindowInfo};
use tmux_agent_sidebar::ui::colors::ColorTheme;
use tmux_agent_sidebar::ui::icons::StatusIcons;

// ─── Agents: auto-scroll behavior Tests ─────────────────────────────

#[test]
fn test_agents_auto_scroll_keeps_selected_visible() {
    // Create enough agents to overflow a small viewport
    let mut panes = Vec::new();
    for i in 0..10 {
        let mut pane = make_pane(AgentType::Claude, PaneStatus::Idle);
        pane.pane_id = format!("%{}", i);
        panes.push(pane);
    }

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: panes.clone(),
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", panes)];
    state.focus_state.sidebar_focused = true;
    state.focus_state.focus = Focus::Panes;
    state.rebuild_row_targets();

    // Render with a small height. With the 2-row header, the first pane
    // still stays visible without needing to scroll.
    let _ = render_to_string(&mut state, 28, 26);
    assert_eq!(state.scrolls.panes.offset, 0, "initially at top");

    // Select last agent and re-render
    state.global.selected_pane_row = 9;
    let _ = render_to_string(&mut state, 28, 26);
    assert!(
        state.scrolls.panes.offset > 0,
        "should scroll down to show selected agent"
    );
}

#[test]
fn test_panes_scroll_offset_tracks_total_and_visible() {
    let pane = make_pane(AgentType::Claude, PaneStatus::Idle);
    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![pane.clone()],
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", vec![pane])];
    state.rebuild_row_targets();

    let _ = render_to_string(&mut state, 28, 26);
    // After rendering, panes_scroll.total_lines and panes_scroll.visible_height should be set
    assert!(
        state.scrolls.panes.total_lines > 0,
        "total lines should be populated"
    );
    assert!(
        state.scrolls.panes.visible_height > 0,
        "visible height should be populated"
    );
}

// ─── Agents: Codex agent color ──────────────────────────────────────

#[test]
fn snapshot_codex_agent_styled() {
    let theme = ColorTheme::default();
    assert_eq!(
        theme.agent_color(&AgentType::Codex),
        ratatui::style::Color::Indexed(141)
    );
}

// ─── Agents: Unknown agent type ─────────────────────────────────────

#[test]
fn snapshot_unknown_agent_styled() {
    let theme = ColorTheme::default();
    assert_eq!(
        theme.agent_color(&AgentType::Unknown),
        ratatui::style::Color::Indexed(244)
    );
}

// ─── Agents: running icon variants via render ───────────────────────

#[test]
fn test_running_icon_blink_off() {
    let pane = make_pane(AgentType::Claude, PaneStatus::Running);
    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![pane.clone()],
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", vec![pane])];
    state.rebuild_row_targets();
    state.focus_state.sidebar_focused = false;
    state.spinner_frame = 0;

    insta::assert_snapshot!(render_to_string(&mut state, 28, 25), @"
    project
    ┃● claude
    ╭ Activity │ Git ──────────╮
    │      No activity yet     │
    ╰──────────────────────────╯
    ");
}

#[test]
fn test_running_spinner_frame_advances() {
    let pane = make_pane(AgentType::Claude, PaneStatus::Running);
    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![pane.clone()],
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", vec![pane])];
    state.rebuild_row_targets();
    state.focus_state.sidebar_focused = false;
    state.spinner_frame = 3;

    insta::assert_snapshot!(render_to_string(&mut state, 28, 25), @"
    project
    ┃● claude
    ╭ Activity │ Git ──────────╮
    │      No activity yet     │
    ╰──────────────────────────╯
    ");
}

#[test]
fn test_waiting_icon() {
    let pane = make_pane(AgentType::Claude, PaneStatus::Waiting);
    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![pane.clone()],
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", vec![pane])];
    state.rebuild_row_targets();
    state.focus_state.sidebar_focused = false;

    insta::assert_snapshot!(render_to_string(&mut state, 28, 25), @"
    project
    ┃◐ claude
    ╭ Activity │ Git ──────────╮
    │      No activity yet     │
    ╰──────────────────────────╯
    ");
}

#[test]
fn test_error_icon() {
    let pane = make_pane(AgentType::Claude, PaneStatus::Error);
    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: vec![pane.clone()],
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", vec![pane])];
    state.rebuild_row_targets();
    state.focus_state.sidebar_focused = false;

    insta::assert_snapshot!(render_to_string(&mut state, 28, 25), @"
    project
    ┃✕ claude
    ╭ Activity │ Git ──────────╮
    │      No activity yet     │
    ╰──────────────────────────╯
    ");
}

#[test]
fn test_unknown_status_icon() {
    let icons = StatusIcons::default();
    assert_eq!(icons.status_icon(&PaneStatus::Unknown), "·");
}

// ─── Agents: auto-scroll keeps selected pane visible ───────────────

#[test]
fn test_agents_auto_scroll_shows_last_selected_pane() {
    // When the last agent in a group is selected, the auto-scroll
    // should bring it into view (the selection marker must be visible).
    let mut panes = Vec::new();
    for i in 0..6 {
        let mut pane = make_pane(AgentType::Claude, PaneStatus::Idle);
        pane.pane_id = format!("%{}", i);
        panes.push(pane);
    }

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: panes.clone(),
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", panes)];
    state.focus_state.sidebar_focused = true;
    state.focus_state.focus = Focus::Panes;
    state.rebuild_row_targets();

    // Select the last agent
    state.global.selected_pane_row = 5;
    // Use a tight height so agents area is small (height - 1 margin - 20 bottom)
    let _ = render_to_string(&mut state, 28, 26);

    // Auto-scroll should have moved forward to keep the last-selected pane visible.
    assert!(
        state.scrolls.panes.offset > 0,
        "selecting the last agent should scroll the list"
    );
}

#[test]
fn test_agents_auto_scroll_up_shows_group_header() {
    // After scrolling down, selecting the first agent should scroll
    // back up enough to show the group header.
    let mut panes = Vec::new();
    for i in 0..8 {
        let mut pane = make_pane(AgentType::Claude, PaneStatus::Idle);
        pane.pane_id = format!("%{}", i);
        panes.push(pane);
    }

    let mut state = make_state(vec![SessionInfo {
        session_name: "main".into(),
        windows: vec![WindowInfo {
            window_id: "@1".into(),
            window_name: "project".into(),
            window_active: true,
            auto_rename: false,
            panes: panes.clone(),
        }],
    }]);
    state.repo_groups = vec![make_repo_group("project", panes)];
    state.focus_state.sidebar_focused = true;
    state.focus_state.focus = Focus::Panes;
    state.rebuild_row_targets();

    // Scroll to bottom
    state.global.selected_pane_row = 7;
    let _ = render_to_string(&mut state, 28, 26);
    assert!(state.scrolls.panes.offset > 0, "should have scrolled down");

    // Now select first agent and re-render
    state.global.selected_pane_row = 0;
    // The snapshot locks in that the `project` repo header is visible after
    // scrolling back up to the first agent.
    insta::assert_snapshot!(render_to_string(&mut state, 28, 26), @"
    project
     ○ claude
       Waiting for prompt…
    ┃○ claude
    ┃  Waiting for prompt…
    ╭ Activity │ Git ──────────╮
    │      No activity yet     │
    ╰──────────────────────────╯
    ");
}

// ─── Repo popup rendering ───────────────────────────────────────────
