//! Main application orchestration: prime the [`AppState`], spawn background
//! workers, and run the crossterm event loop. Split out from `src/main.rs` so
//! the binary entry point only handles CLI arg parsing, signal wiring, and
//! TUI session setup.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{self};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::SPINNER_PULSE;
use crate::state::BottomTab;

mod input;
mod render;
mod setup;
mod workers;

/// Run the TUI event loop. Returns when the loop exits (currently only on
/// fatal I/O error, since the loop is `loop { ... }`).
///
/// `needs_refresh` is the process-wide SIGUSR1 flag owned by `main.rs` — the
/// signal handler must reference a static visible at signal-handler time,
/// so the static stays with the `extern "C"` handler in the binary crate and
/// we just borrow it here.
pub fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    tmux_pane: String,
    needs_refresh: &'static AtomicBool,
) -> io::Result<()> {
    let mut state = setup::init_state(tmux_pane);
    let mut window_inactive_count: u32 = 0;

    let workers = workers::spawn(&state);
    let workers::Workers {
        git_rx,
        session_rx,
        git_tab_active,
    } = workers;

    let mut last_refresh = std::time::Instant::now();
    let mut last_spinner = std::time::Instant::now();
    let spinner_interval = Duration::from_millis(200);
    let mut needs_redraw = true;

    loop {
        let refresh_interval = refresh_interval_for(window_inactive_count);
        let hidden = is_hidden(window_inactive_count);
        if needs_redraw {
            render::render_frame(terminal, &mut state)?;
            needs_redraw = false;
        }

        let timeout = poll_timeout(
            needs_refresh.load(Ordering::Relaxed),
            hidden,
            refresh_interval.saturating_sub(last_refresh.elapsed()),
            spinner_interval.saturating_sub(last_spinner.elapsed()),
        );
        if event::poll(timeout)? {
            loop {
                let ev = event::read()?;
                if input::handle_event(ev, &mut state, &git_tab_active, terminal) {
                    needs_redraw = true;
                }
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }

        // Fork-local: paint the clicked row as active before the tmux jump,
        // which is several blocking subprocesses and felt like a freeze.
        if state.focus_state.pending_activation.is_some() {
            render::render_frame(terminal, &mut state)?;
            needs_redraw = false;
            state.run_pending_activation();
        }

        // Fork-local: a sidebar in a window nobody is looking at does not
        // animate; the spinner only costs tmux and terminal redraws there.
        if !hidden && last_spinner.elapsed() >= spinner_interval {
            state.spinner_frame = (state.spinner_frame + 1) % SPINNER_PULSE.len();
            if state.pet_enabled {
                let term_width = terminal.size().map(|s| s.width).unwrap_or(60);
                state.tick_pet(term_width);
            }
            last_spinner = std::time::Instant::now();
            needs_redraw = true;
        }

        let sigusr1 = needs_refresh.swap(false, Ordering::Relaxed);
        if sigusr1 || last_refresh.elapsed() >= refresh_interval {
            let previous_focused_pane_id = state.focus_state.focused_pane_id.clone();
            // Fork-local: SIGUSR1 means focus moved. The full refresh below
            // snapshots every process and is slow under load, so move the
            // highlight with one cheap tmux query and paint it first.
            if sigusr1 {
                state.find_focused_pane();
                if state.focus_state.focused_pane_id != previous_focused_pane_id {
                    render::render_frame(terminal, &mut state)?;
                }
            }
            // Fork-local: a hidden sidebar reads what the visible one just
            // collected instead of running the same commands itself. A
            // signalled refresh means somebody is looking, so it wants
            // fresh data.
            crate::shared_cache::set_max_age(shared_max_age(sigusr1, hidden));
            let is_window_active = state.refresh();
            if state.focus_state.focused_pane_id != previous_focused_pane_id {
                render::refresh_git_for_focused_pane(&mut state);
            }
            needs_redraw = true;
            if is_window_active {
                if window_inactive_count >= 2 {
                    state.global.load_from_tmux();
                    state.rebuild_row_targets();
                }
                window_inactive_count = 0;
            } else {
                window_inactive_count = window_inactive_count.saturating_add(1);
            }
            git_tab_active.store(
                git_poll_wanted(
                    state.bottom_tab == BottomTab::GitStatus,
                    is_hidden(window_inactive_count),
                ),
                Ordering::Relaxed,
            );
            last_refresh = std::time::Instant::now();
        }

        if let Ok(data) = git_rx.try_recv() {
            state.apply_git_data(data);
            needs_redraw = true;
        }

        if let Ok(names) = session_rx.try_recv() {
            state.sessions.names = names;
            state.sessions.dirty = true;
            needs_redraw = true;
        }

        state
            .global
            .flush_pending_cursor_save(std::time::Duration::from_millis(120));
    }
}

/// Refreshes in a row that found the sidebar's window inactive before the
/// sidebar counts as hidden.
const HIDDEN_AFTER_INACTIVE_REFRESHES: u32 = 2;

/// Fork-local: how often the full refresh runs. Every refresh snapshots all
/// processes and runs `ps`, several `tmux` calls and one `git` per agent pane,
/// and every window carries its own sidebar, so a dozen hidden sidebars
/// refreshing every second kept the Mac busy with ~1,500 new processes a
/// second (Agenda #806, 11. 11. 2026). A hidden sidebar refreshes every 10 s;
/// switching to its window sends SIGUSR1, which refreshes it at once.
fn refresh_interval_for(window_inactive_count: u32) -> Duration {
    if is_hidden(window_inactive_count) {
        Duration::from_secs(10)
    } else {
        Duration::from_secs(1)
    }
}

fn is_hidden(window_inactive_count: u32) -> bool {
    window_inactive_count >= HIDDEN_AFTER_INACTIVE_REFRESHES
}

/// Fork-local: whether the background git poll (about eight `git` runs every
/// 2 s) should run. A hidden sidebar never shows its Git tab, and twelve of
/// them polling were most of ~1,000 new processes a second (11. 11. 2026).
/// Switching back to the window refreshes at once and turns the poll back on.
fn git_poll_wanted(git_tab_shown: bool, hidden: bool) -> bool {
    git_tab_shown && !hidden
}

/// Fork-local: how old shared command output this refresh accepts.
fn shared_max_age(signalled: bool, hidden: bool) -> Duration {
    if hidden && !signalled {
        crate::shared_cache::HIDDEN_MAX_AGE
    } else {
        crate::shared_cache::VISIBLE_MAX_AGE
    }
}

/// How long the loop waits for input before the next pass. A hidden sidebar
/// never advances its spinner, so its spinner deadline stays in the past;
/// counting it would poll with a zero timeout and spin a core (11. 11. 2026).
fn poll_timeout(
    refresh_pending: bool,
    hidden: bool,
    refresh_timeout: Duration,
    spinner_timeout: Duration,
) -> Duration {
    if refresh_pending {
        return Duration::ZERO;
    }
    let timeout = refresh_timeout.min(Duration::from_millis(16));
    if hidden {
        timeout
    } else {
        timeout.min(spinner_timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_sidebar_refreshes_every_second() {
        assert_eq!(refresh_interval_for(0), Duration::from_secs(1));
        assert_eq!(refresh_interval_for(1), Duration::from_secs(1));
        assert!(!is_hidden(1));
    }

    #[test]
    fn hidden_sidebar_refreshes_every_ten_seconds() {
        assert!(is_hidden(2));
        assert_eq!(refresh_interval_for(2), Duration::from_secs(10));
        assert_eq!(refresh_interval_for(u32::MAX), Duration::from_secs(10));
    }

    #[test]
    fn hidden_sidebar_does_not_spin_on_a_stale_spinner() {
        // A hidden sidebar never advances its spinner, so the spinner
        // deadline is long past; the loop must still sleep between passes.
        let timeout = poll_timeout(false, true, Duration::from_secs(9), Duration::ZERO);
        assert_eq!(timeout, Duration::from_millis(16));
    }

    #[test]
    fn visible_sidebar_wakes_for_its_spinner() {
        let timeout = poll_timeout(false, false, Duration::from_secs(1), Duration::ZERO);
        assert_eq!(timeout, Duration::ZERO);
    }

    #[test]
    fn hidden_sidebar_does_not_poll_git() {
        // Twelve hidden sidebars polling git every 2 s were most of the
        // ~1,000 new processes a second on 11. 11. 2026.
        assert!(!git_poll_wanted(true, true));
    }

    #[test]
    fn visible_git_tab_polls_git() {
        assert!(git_poll_wanted(true, false));
        assert!(!git_poll_wanted(false, false));
    }

    #[test]
    fn hidden_sidebar_reads_older_shared_output_than_a_visible_one() {
        use crate::shared_cache::{HIDDEN_MAX_AGE, VISIBLE_MAX_AGE};
        assert_eq!(shared_max_age(false, true), HIDDEN_MAX_AGE);
        assert_eq!(shared_max_age(false, false), VISIBLE_MAX_AGE);
        // Switching to the window signals the sidebar: fresh data at once.
        assert_eq!(shared_max_age(true, true), VISIBLE_MAX_AGE);
        assert!(HIDDEN_MAX_AGE > Duration::from_secs(10));
    }

    #[test]
    fn pending_refresh_polls_without_waiting() {
        assert_eq!(
            poll_timeout(true, true, Duration::from_secs(9), Duration::from_secs(1)),
            Duration::ZERO
        );
    }
}
