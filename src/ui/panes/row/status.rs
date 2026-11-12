use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

use super::ctx::RowCtx;
use crate::tmux::PaneStatus;
use crate::ui::icons::StatusIcons;
use crate::ui::text::{display_width, elapsed_label, truncate_to_width};

/// What the Playground hook puts before the name of a wrapped-up chat.
const WRAPPED_PREFIX: &str = "✔ ";
const WRAPPED_ICON: &str = "✔";

pub(super) fn status_row(
    pane: &crate::tmux::PaneInfo,
    ctx: &RowCtx,
    icons: &StatusIcons,
    spinner_frame: usize,
    now: u64,
) -> Line<'static> {
    use crate::tmux::PermissionMode;
    let theme = ctx.theme;

    // Fork-local (11. 11. 2026, owner decision #1165): a chat wrapped up in its
    // last turn carries "✔ <title>" in @pane_name, written by the Playground hook
    // until the next prompt. The mark takes the place of the idle circle (the chat
    // is free for a new topic) and is cut from the name, so it shows once. A
    // working or waiting agent keeps its own icon.
    let wrapped_title = pane.custom_name.strip_prefix(WRAPPED_PREFIX);
    let wrapped = wrapped_title.is_some() && matches!(pane.status, PaneStatus::Idle);
    let (icon, pulse_color) = if wrapped {
        (WRAPPED_ICON, None)
    } else {
        running_icon_for(&pane.status, spinner_frame, icons)
    };
    let icon_color = if wrapped {
        Color::Indexed(15)
    } else {
        pulse_color.unwrap_or_else(|| theme.status_color(&pane.status, pane.attention))
    };
    // Fork-local: picker-assigned @pane_name wins over the scanned
    // Claude /rename session name (and updates every refresh, not only
    // when the session map goes dirty).
    let title_raw: &str = if let Some(title) = wrapped_title {
        title
    } else if !pane.custom_name.is_empty() {
        &pane.custom_name
    } else if pane.session_name.is_empty() {
        pane.agent.label()
    } else {
        &pane.session_name
    };
    let badge = pane.permission_mode.badge();
    let elapsed = elapsed_label(pane.started_at, now);

    // Fork-local (12. 11. 2026): the title has the colour of the window tab
    // for the same state, so a working agent reads the same in both places.
    // ~~a named instance renders bright green, the plain label keeps the
    // agent colour~~ (3. 9. to 12. 11. 2026).
    let title_fg = title_color(&pane.status, pane.attention);
    let elapsed_fg = if pane.status.is_active() {
        theme.text_active
    } else {
        theme.text_muted
    };

    // Fork-local: 1-based jump index (Alt+q..u order) before the icon.
    let num = if pane.ordinal > 0 {
        format!("{} ", pane.ordinal)
    } else {
        String::new()
    };

    let badge_extra = if badge.is_empty() { 0 } else { 1 };
    let fixed_width =
        display_width(&num) + display_width(icon) + 1 + badge_extra + display_width(badge);
    // User-supplied session names (set via `/rename`) can be arbitrarily
    // long; cap the title to the space left after reserving room for the
    // icon, badge, and elapsed label so they stay visible instead of
    // being pushed off-screen.
    let elapsed_width = display_width(&elapsed);
    let elapsed_gap = usize::from(elapsed_width > 0);
    let title_budget = ctx
        .inner_width
        .saturating_sub(fixed_width + elapsed_gap + elapsed_width);
    // Fork-local: Playground car mode puts the agent's own car before its name.
    let title_full = if pane.car_emoji.is_empty() {
        title_raw.to_string()
    } else {
        format!("{} {}", pane.car_emoji, title_raw)
    };
    let title = truncate_to_width(&title_full, title_budget);

    let left_width = fixed_width + display_width(&title);
    let available_for_elapsed = ctx.inner_width.saturating_sub(left_width);
    let elapsed = truncate_to_width(&elapsed, available_for_elapsed);
    let elapsed_width = display_width(&elapsed);

    let mut left_spans: Vec<Span<'static>> = Vec::with_capacity(4);
    if !num.is_empty() {
        left_spans.push(Span::styled(
            num.clone(),
            // bright yellow, not grey — Martin reads these at a glance
            ctx.apply_bg(Style::default().fg(Color::Indexed(11))),
        ));
    }
    left_spans.push(Span::styled(
        icon.to_string(),
        ctx.apply_bg(Style::default().fg(icon_color)),
    ));
    left_spans.push(Span::styled(
        format!(" {}", title),
        ctx.apply_bg(Style::default().fg(title_fg)),
    ));
    if !badge.is_empty() {
        let badge_color = match pane.permission_mode {
            PermissionMode::BypassPermissions => theme.badge_danger,
            PermissionMode::Auto => theme.badge_auto,
            PermissionMode::DontAsk => theme.badge_auto,
            PermissionMode::Plan => theme.badge_plan,
            PermissionMode::AcceptEdits => theme.badge_auto,
            PermissionMode::Defer => theme.badge_auto,
            PermissionMode::Default => theme.text_muted,
        };
        left_spans.push(Span::styled(
            format!(" {}", badge),
            ctx.apply_bg(Style::default().fg(badge_color)),
        ));
    }

    let right_spans = vec![Span::styled(
        elapsed,
        ctx.apply_bg(Style::default().fg(elapsed_fg)),
    )];

    ctx.row_line_split(left_spans, left_width, right_spans, elapsed_width)
}

pub(super) fn running_icon_for<'a>(
    status: &PaneStatus,
    spinner_frame: usize,
    icons: &'a StatusIcons,
) -> (&'a str, Option<Color>) {
    use crate::SPINNER_PULSE;

    match status {
        PaneStatus::Running => {
            let color_idx = SPINNER_PULSE[spinner_frame % SPINNER_PULSE.len()];
            (icons.status_icon(status), Some(Color::Indexed(color_idx)))
        }
        _ => (icons.status_icon(status), None),
    }
}

/// Fork-local (12. 11. 2026): the agent title in the colour its window tab
/// has for the same state (`window-status-format` in ~/.tmux.conf): working
/// is bright green (10), waiting for the owner or failed is bright red (9),
/// everything else is the cyan of an inactive tab (6).
pub(super) fn title_color(status: &PaneStatus, attention: bool) -> Color {
    if attention {
        return Color::Indexed(9);
    }
    match status {
        PaneStatus::Running | PaneStatus::Background => Color::Indexed(10),
        PaneStatus::Waiting | PaneStatus::Error => Color::Indexed(9),
        PaneStatus::Idle | PaneStatus::Unknown => Color::Indexed(6),
    }
}

#[cfg(test)]
mod title_color_tests {
    use super::*;

    #[test]
    fn working_agent_title_is_green_like_its_tab() {
        assert_eq!(title_color(&PaneStatus::Running, false), Color::Indexed(10));
        assert_eq!(
            title_color(&PaneStatus::Background, false),
            Color::Indexed(10)
        );
    }

    #[test]
    fn idle_agent_title_is_cyan_like_an_inactive_tab() {
        assert_eq!(title_color(&PaneStatus::Idle, false), Color::Indexed(6));
        assert_eq!(title_color(&PaneStatus::Unknown, false), Color::Indexed(6));
    }

    #[test]
    fn waiting_or_attention_title_is_red_like_its_tab() {
        assert_eq!(title_color(&PaneStatus::Waiting, false), Color::Indexed(9));
        assert_eq!(title_color(&PaneStatus::Idle, true), Color::Indexed(9));
        assert_eq!(title_color(&PaneStatus::Error, false), Color::Indexed(9));
    }
}
