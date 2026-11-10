use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::ctx::RowCtx;
use crate::tmux::PaneStatus;
use crate::ui::text::{display_width, wait_reason_label, wrap_text, wrap_text_char};

pub(super) fn task_progress_row(
    task_progress: Option<&crate::activity::TaskProgress>,
    ctx: &RowCtx,
) -> Option<Line<'static>> {
    use crate::activity::TaskStatus;
    let progress = task_progress?;
    if progress.is_empty() {
        return None;
    }

    let mut icons = String::with_capacity(progress.tasks.len() * 3);
    for (_, status) in &progress.tasks {
        let ch = match status {
            TaskStatus::Completed => "✔",
            TaskStatus::InProgress => "◼",
            TaskStatus::Pending => "◻",
        };
        icons.push_str(ch);
    }
    let summary = format!(
        "  {} {}/{}",
        icons,
        progress.completed_count(),
        progress.total()
    );
    let summary_dw = display_width(&summary);
    let task_color = ctx.theme.task_progress;
    Some(ctx.row_line(
        vec![Span::styled(
            summary,
            ctx.apply_bg(Style::default().fg(task_color)),
        )],
        summary_dw,
    ))
}

pub(super) fn wait_reason_row(
    wait_reason: &str,
    status: &PaneStatus,
    ctx: &RowCtx,
) -> Option<Line<'static>> {
    if wait_reason.is_empty() {
        return None;
    }
    // "resumed" / "resumed (compact)" only state that the session was
    // restored — no action is needed, so the row is pure noise. Actionable
    // reasons (permission required, rate limit, ...) still render.
    if wait_reason.starts_with("session_resumed") {
        return None;
    }
    let reason = wait_reason_label(wait_reason);
    let text = format!("  {}", reason);
    let text_dw = display_width(&text);
    let reason_color = if matches!(status, PaneStatus::Error) {
        ctx.theme.status_error
    } else {
        ctx.theme.wait_reason
    };
    Some(ctx.row_line(
        vec![Span::styled(
            text,
            ctx.apply_bg(Style::default().fg(reason_color)),
        )],
        text_dw,
    ))
}

/// Fork-local: the faces of the Playground team members. A reply opens with its
/// member's face on a line of its own (rules-owner, 6. 11. 2026); the agent row
/// shows the same face from `@team_face`, so the response line leaves it out.
const TEAM_FACES: [&str; 3] = ["👩🏻", "👱🏻\u{200d}♂\u{fe0f}", "👩🏼\u{200d}🦰"];

fn without_team_face(text: &str) -> &str {
    let mut rest = text.trim_start();
    while let Some(face) = TEAM_FACES.iter().find(|f| rest.starts_with(**f)) {
        rest = rest[face.len()..].trim_start();
    }
    rest
}

pub(super) fn prompt_rows(pane: &crate::tmux::PaneInfo, ctx: &RowCtx) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let is_response = pane.prompt_is_response;
    let text = if is_response {
        without_team_face(&pane.prompt)
    } else {
        pane.prompt.as_str()
    };
    let prompt_color = if ctx.active {
        theme.text_active
    } else {
        theme.text_inactive
    };
    let wrap_width = ctx.inner_width.saturating_sub(2);
    let wrapped = if is_response {
        wrap_text_char(text, wrap_width, 3)
    } else {
        wrap_text(text, wrap_width, 3)
    };

    // Fork-local (owner, 10. 11. 2026): the two columns in front of the
    // first line, under the agent's ordinal, carry the number of running
    // subagents. Their names were noise and are not listed any more, and
    // the response arrow that used to sit here said nothing the owner read.
    let subagents = pane.subagents.len();
    let mut out = Vec::with_capacity(wrapped.len());
    for (li, wl) in wrapped.iter().enumerate() {
        if li == 0 && subagents > 0 {
            let count = if subagents > 9 {
                "9+".to_string()
            } else {
                format!("{subagents} ")
            };
            let text_dw = 2 + display_width(wl);
            out.push(ctx.row_line(
                vec![
                    Span::styled(
                        count,
                        ctx.apply_bg(
                            Style::default()
                                .fg(theme.subagent)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ),
                    Span::styled(wl.clone(), ctx.apply_bg(Style::default().fg(prompt_color))),
                ],
                text_dw,
            ));
        } else {
            let indent = "  ";
            let text = format!("{}{}", indent, wl);
            let text_dw = display_width(&text);
            out.push(ctx.row_line(
                vec![Span::styled(
                    text,
                    ctx.apply_bg(Style::default().fg(prompt_color)),
                )],
                text_dw,
            ));
        }
    }
    out
}

pub(super) fn idle_hint_row(ctx: &RowCtx) -> Line<'static> {
    let text = "  Waiting for prompt…";
    let text_dw = display_width(text);
    let idle_color = if ctx.active {
        ctx.theme.text_active
    } else {
        ctx.theme.text_inactive
    };
    ctx.row_line(
        vec![Span::styled(
            text.to_string(),
            ctx.apply_bg(Style::default().fg(idle_color)),
        )],
        text_dw,
    )
}
