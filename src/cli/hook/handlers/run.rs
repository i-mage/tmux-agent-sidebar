use crate::cli::{sanitize_tmux_value, set_attention, set_status};
use crate::desktop_notification;
use crate::desktop_notification::DesktopNotificationKind;
use crate::tmux;

use crate::time::now_epoch_secs;

use super::super::context::{
    AgentContext, clear_run_state, is_system_message, mark_task_reset, set_agent_meta,
};
use super::super::notifications::{
    NotifyLabels, NotifyPayload, notification_run_id, notify_lifecycle, set_notification_run_id,
    stop_body, stop_failure_body, stop_failure_fingerprint, task_completed_body,
    task_completed_fingerprint,
};
use super::status_priority::resolve_stop_status;

pub(in crate::cli::hook) fn on_user_prompt_submit(
    pane: &str,
    ctx: &AgentContext<'_>,
    prompt: &str,
) -> i32 {
    set_agent_meta(pane, ctx);
    set_attention(pane, "clear");
    set_status(pane, "running");
    set_notification_run_id(pane);
    if !prompt.is_empty() && !is_system_message(prompt) {
        let p = sanitize_tmux_value(prompt);
        tmux::set_pane_option(pane, tmux::PANE_PROMPT, &p);
        tmux::set_pane_option(pane, tmux::PANE_PROMPT_SOURCE, "user");
    }
    tmux::set_pane_option(pane, tmux::PANE_STARTED_AT, &now_epoch_secs().to_string());
    tmux::unset_pane_option(pane, tmux::PANE_WAIT_REASON);
    0
}

/// A subagent whose transcript was written this recently is still running.
/// Long enough to cover a test suite or a build it is waiting on, short
/// enough that an entry left by a missed SubagentStop clears on a later Stop.
const SUBAGENT_LIVE_SECS: u64 = 600;

fn claude_projects_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(std::path::Path::new(&home).join(".claude").join("projects"))
}

/// The entries of a `@pane_subagents` list (`type:id,type:id`) whose
/// transcript `<projects>/<project>/<session>/subagents/agent-<id>.jsonl`
/// was modified within [`SUBAGENT_LIVE_SECS`] of `now`. Without a session id
/// nothing can be checked, so nothing is kept.
fn live_subagents(
    current: &str,
    session_id: Option<&str>,
    projects: &std::path::Path,
    now: std::time::SystemTime,
) -> String {
    let Some(session) = session_id.filter(|s| !s.is_empty()) else {
        return String::new();
    };
    let dirs: Vec<std::path::PathBuf> = std::fs::read_dir(projects)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.path().join(session).join("subagents"))
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    let is_live = |id: &str| {
        dirs.iter().any(|dir| {
            std::fs::metadata(dir.join(format!("agent-{id}.jsonl")))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|written| now.duration_since(written).ok())
                .is_some_and(|age| age.as_secs() <= SUBAGENT_LIVE_SECS)
        })
    };
    current
        .split(',')
        .filter(|entry| {
            entry
                .rsplit_once(':')
                .is_some_and(|(_, id)| !id.is_empty() && is_live(id))
        })
        .collect::<Vec<_>>()
        .join(",")
}

pub(in crate::cli::hook) fn on_stop(
    pane: &str,
    ctx: &AgentContext<'_>,
    last_message: &str,
    response: Option<&str>,
    notifications: &desktop_notification::DesktopNotificationSettings,
) -> i32 {
    set_agent_meta(pane, ctx);
    set_attention(pane, "clear");
    if !last_message.is_empty() {
        let msg = sanitize_tmux_value(last_message);
        tmux::set_pane_option(pane, tmux::PANE_PROMPT, &msg);
        tmux::set_pane_option(pane, tmux::PANE_PROMPT_SOURCE, "response");
    }
    let bg_shell_live = !tmux::get_pane_option_value(pane, tmux::PANE_BG_CMD).is_empty();
    // `Stop` is emitted for the parent turn. A synchronous `Task` subagent
    // is over by then, so a leftover entry is stale state from a missed or
    // mismatched SubagentStop. Fork-local (10. 11. 2026): a BACKGROUND
    // subagent is not over. It keeps running after the parent's Stop, and
    // clearing the whole list made the sidebar show a chat with a running
    // subagent as having none. An entry stays while its transcript is still
    // being written; the rest is dropped before `mark_task_reset`, whose
    // guard intentionally skips writes while subagents are active.
    let current = tmux::get_pane_option_value(pane, tmux::PANE_SUBAGENTS);
    if !current.is_empty() {
        let kept = match claude_projects_dir() {
            Some(root) => live_subagents(
                &current,
                ctx.session_id.as_deref(),
                &root,
                std::time::SystemTime::now(),
            ),
            None => String::new(),
        };
        if kept.is_empty() {
            tmux::unset_pane_option(pane, tmux::PANE_SUBAGENTS);
        } else if kept != current {
            tmux::set_pane_option(pane, tmux::PANE_SUBAGENTS, &kept);
        }
    }
    if bg_shell_live {
        tmux::unset_pane_option(pane, tmux::PANE_WAIT_REASON);
    } else {
        clear_run_state(pane);
    }
    mark_task_reset(pane);
    set_status(pane, resolve_stop_status(bg_shell_live));

    if !bg_shell_live {
        let run_id = notification_run_id(pane);
        // Skip the generic Stop notification if an explicit TaskCompleted
        // stamp from the current run has already fired — otherwise Claude
        // Code's `TaskCompleted` → `Stop` sequence produces two desktop
        // notifications for the same logical completion.
        let already_notified = desktop_notification::has_run_scoped_stamp(
            pane,
            DesktopNotificationKind::TaskCompleted,
            run_id,
        );
        if !already_notified {
            let _ = notify_lifecycle(
                pane,
                NotifyLabels::FromCtx(ctx),
                notifications,
                run_id,
                NotifyPayload {
                    kind: DesktopNotificationKind::TaskCompleted,
                    event: desktop_notification::DesktopNotificationEvent::Stop,
                    fingerprint_suffix: "stop",
                    body: &stop_body(last_message),
                },
            );
        }
    }
    if let Some(resp) = response {
        println!("{resp}");
    }
    0
}

pub(in crate::cli::hook) fn on_stop_failure(
    pane: &str,
    ctx: &AgentContext<'_>,
    error: &str,
    notifications: &desktop_notification::DesktopNotificationSettings,
) -> i32 {
    set_agent_meta(pane, ctx);
    set_attention(pane, "clear");
    clear_run_state(pane);
    mark_task_reset(pane);
    if !error.is_empty() {
        tmux::set_pane_option(pane, tmux::PANE_WAIT_REASON, error);
    }
    set_status(pane, "error");
    let _ = notify_lifecycle(
        pane,
        NotifyLabels::FromCtx(ctx),
        notifications,
        None,
        NotifyPayload {
            kind: DesktopNotificationKind::TaskFailed,
            event: desktop_notification::DesktopNotificationEvent::StopFailure,
            fingerprint_suffix: stop_failure_fingerprint(error),
            body: &stop_failure_body(error),
        },
    );
    0
}

pub(in crate::cli::hook) fn on_task_completed(
    pane: &str,
    agent_name: &str,
    task_id: &str,
    task_subject: &str,
    notifications: &desktop_notification::DesktopNotificationSettings,
) -> i32 {
    let _ = notify_lifecycle(
        pane,
        NotifyLabels::FromPane { agent: agent_name },
        notifications,
        None,
        NotifyPayload {
            kind: DesktopNotificationKind::TaskCompleted,
            event: desktop_notification::DesktopNotificationEvent::TaskCompleted,
            fingerprint_suffix: task_completed_fingerprint(task_id, task_subject),
            body: &task_completed_body(task_subject),
        },
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_user_prompt_submit_sets_running_and_stores_prompt() {
        let _guard = tmux::test_mock::install();
        let pane = "%PROMPT";
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let exit = on_user_prompt_submit(pane, &ctx, "fix the bug");
        assert_eq!(exit, 0);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("running")
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_PROMPT).as_deref(),
            Some("fix the bug")
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_PROMPT_SOURCE).as_deref(),
            Some("user")
        );
        assert!(tmux::test_mock::contains(pane, tmux::PANE_STARTED_AT));
    }

    #[test]
    fn on_user_prompt_submit_ignores_system_messages() {
        let _guard = tmux::test_mock::install();
        let pane = "%SYS_PROMPT";
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        on_user_prompt_submit(pane, &ctx, "<system-reminder>ignore me</system-reminder>");
        assert!(
            !tmux::test_mock::contains(pane, tmux::PANE_PROMPT),
            "system messages should not be stored as user prompt"
        );
        // But status should still advance to running.
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("running")
        );
    }

    #[test]
    fn on_user_prompt_submit_clears_stale_wait_reason_but_preserves_bg_cmd() {
        let _guard = tmux::test_mock::install();
        let pane = "%PROMPT_CLEAR_WAIT";
        tmux::test_mock::set(pane, tmux::PANE_WAIT_REASON, "permission");
        tmux::test_mock::set(pane, tmux::PANE_BG_CMD, "npm run dev");
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        on_user_prompt_submit(pane, &ctx, "new prompt");
        assert!(!tmux::test_mock::contains(pane, tmux::PANE_WAIT_REASON));
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_BG_CMD).as_deref(),
            Some("npm run dev"),
            "bg command must survive a new user turn — the shell is still running",
        );
    }

    #[test]
    fn on_stop_with_background_shell_sets_background_status() {
        let _guard = tmux::test_mock::install();
        let pane = "%STOP_BG";
        tmux::test_mock::set(pane, tmux::PANE_BG_CMD, "npm run dev");
        tmux::test_mock::set(pane, tmux::PANE_STARTED_AT, "123");
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };

        let exit = on_stop(
            pane,
            &ctx,
            "",
            None,
            &desktop_notification::DesktopNotificationSettings {
                enabled: false,
                events: Default::default(),
            },
        );

        assert_eq!(exit, 0);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("background")
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STARTED_AT).as_deref(),
            Some("123")
        );
    }

    #[test]
    fn on_stop_without_background_shell_sets_idle_status() {
        let _guard = tmux::test_mock::install();
        let pane = "%STOP_IDLE";
        tmux::test_mock::set(pane, tmux::PANE_STARTED_AT, "123");
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };

        on_stop(
            pane,
            &ctx,
            "",
            None,
            &desktop_notification::DesktopNotificationSettings {
                enabled: false,
                events: Default::default(),
            },
        );

        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("idle")
        );
        assert!(!tmux::test_mock::contains(pane, tmux::PANE_STARTED_AT));
    }

    // Fork-local (10. 11. 2026): a background subagent outlives the parent's
    // Stop. It stays in the list while its transcript is still being written.

    fn subagent_transcript(root: &std::path::Path, session: &str, id: &str) -> std::path::PathBuf {
        let dir = root.join("-Users-x-proj").join(session).join("subagents");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("agent-{id}.jsonl"));
        std::fs::write(&file, "{}\n").unwrap();
        file
    }

    #[test]
    fn live_subagents_keeps_the_one_still_writing_and_drops_the_silent_one() {
        let root = tempfile::tempdir().unwrap();
        let live = subagent_transcript(root.path(), "sess-1", "aa6e1");
        let stale = subagent_transcript(root.path(), "sess-1", "a0551");
        let now = std::time::SystemTime::now();
        let old = now - std::time::Duration::from_secs(SUBAGENT_LIVE_SECS + 60);
        std::fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(live.exists());

        let kept = live_subagents(
            "general-purpose:aa6e1,general-purpose:a0551,Explore:nofile",
            Some("sess-1"),
            root.path(),
            now,
        );

        assert_eq!(kept, "general-purpose:aa6e1");
    }

    #[test]
    fn live_subagents_of_another_session_do_not_count() {
        let root = tempfile::tempdir().unwrap();
        subagent_transcript(root.path(), "other-session", "aa6e1");

        let kept = live_subagents(
            "general-purpose:aa6e1",
            Some("sess-1"),
            root.path(),
            std::time::SystemTime::now(),
        );

        assert_eq!(kept, "");
    }

    #[test]
    fn live_subagents_without_a_session_id_keeps_nothing() {
        let root = tempfile::tempdir().unwrap();
        subagent_transcript(root.path(), "sess-1", "aa6e1");

        let kept = live_subagents(
            "general-purpose:aa6e1",
            None,
            root.path(),
            std::time::SystemTime::now(),
        );

        assert_eq!(kept, "");
    }

    #[test]
    fn on_stop_clears_stale_subagents() {
        let _guard = tmux::test_mock::install();
        let pane = "%STOP_STALE_SUBAGENTS";
        tmux::test_mock::set(
            pane,
            tmux::PANE_SUBAGENTS,
            "general-purpose:sub-1,general-purpose:sub-2",
        );
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };

        on_stop(
            pane,
            &ctx,
            "",
            None,
            &desktop_notification::DesktopNotificationSettings {
                enabled: false,
                events: Default::default(),
            },
        );

        assert!(
            !tmux::test_mock::contains(pane, tmux::PANE_SUBAGENTS),
            "parent Stop must clear stale subagent list"
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("idle")
        );
    }

    #[test]
    fn on_stop_failure_records_error_wait_reason_and_error_status() {
        let _guard = tmux::test_mock::install();
        let pane = "%STOP_FAIL";
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let exit = on_stop_failure(
            pane,
            &ctx,
            "rate_limit",
            &desktop_notification::DesktopNotificationSettings {
                enabled: false,
                events: Default::default(),
            },
        );
        assert_eq!(exit, 0);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("error")
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_WAIT_REASON).as_deref(),
            Some("rate_limit")
        );
    }
}
