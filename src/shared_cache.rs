//! Fork-local: one refresh for every sidebar (owner decision #1153,
//! 11. 11. 2026).
//!
//! Every tmux window carries its own sidebar process and each used to run the
//! same global commands on its own: `tmux list-panes -a`, `ps` and one
//! `git rev-parse` per repo. With a dozen windows that was a few hundred new
//! processes a second for data that is identical in every sidebar.
//!
//! The output of those commands is now shared through small files in a
//! per-user cache directory. The first sidebar that needs a command runs it
//! and writes the output; every other sidebar that asks within the allowed
//! age reads the file and starts no process. A hidden sidebar accepts older
//! data than a visible one, so while one sidebar is on screen the hidden ones
//! run nothing at all.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Oldest shared output a sidebar somebody is looking at accepts. It
/// refreshes every second, so this keeps its data as fresh as before.
pub const VISIBLE_MAX_AGE: Duration = Duration::from_millis(900);

/// Oldest shared output a hidden sidebar accepts. It refreshes every 10 s and
/// nobody sees it; switching to its window refreshes it with
/// [`VISIBLE_MAX_AGE`] at once.
pub const HIDDEN_MAX_AGE: Duration = Duration::from_secs(15);

/// Branch and repo root of a pane path change rarely, so even a visible
/// sidebar accepts them this old.
pub const GIT_MIN_AGE: Duration = Duration::from_secs(3);

static MAX_AGE_MS: AtomicU64 = AtomicU64::new(900);
/// Shared output written before this moment is stale for this process: it
/// changed tmux state itself and must not read a snapshot from before that.
static NOT_BEFORE_MS: AtomicU64 = AtomicU64::new(0);
/// Only the long-running sidebar turns sharing on; one-shot CLI commands
/// (hooks, toggle) always run their commands directly.
static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

/// Set how old shared output may be for the refresh that follows.
pub fn set_max_age(age: Duration) {
    MAX_AGE_MS.store(age.as_millis() as u64, Ordering::Relaxed);
}

/// Call after this process changed tmux state.
pub fn invalidate() {
    NOT_BEFORE_MS.store(now_ms(), Ordering::Relaxed);
}

/// Return the shared output for `key`, running `run` only when no other
/// sidebar produced it within the allowed age. `min_age` raises the allowed
/// age for data that changes slowly. A failed run is never shared.
pub fn shared_output(
    key: &str,
    min_age: Duration,
    run: impl FnOnce() -> Option<String>,
) -> Option<String> {
    if cfg!(test) || !ENABLED.load(Ordering::Relaxed) {
        return run();
    }
    let Some(dir) = cache_dir() else {
        return run();
    };
    let max_age = Duration::from_millis(MAX_AGE_MS.load(Ordering::Relaxed)).max(min_age);
    shared_output_in(
        &dir,
        key,
        max_age,
        NOT_BEFORE_MS.load(Ordering::Relaxed),
        now_ms(),
        run,
    )
}

fn shared_output_in(
    dir: &Path,
    key: &str,
    max_age: Duration,
    not_before_ms: u64,
    now_ms: u64,
    run: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let path = dir.join(file_name(key));
    if let Some(written_ms) = modified_ms(&path)
        && written_ms >= not_before_ms
        && now_ms.saturating_sub(written_ms) <= max_age.as_millis() as u64
        && let Ok(content) = std::fs::read_to_string(&path)
    {
        return Some(content);
    }
    let output = run()?;
    // Write beside the target and rename, so a reader never sees half a file.
    let tmp = dir.join(format!("{}.{}.tmp", file_name(key), std::process::id()));
    if std::fs::write(&tmp, &output).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Some(output)
}

fn file_name(key: &str) -> String {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn modified_ms(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Per-user directory, private to the user. Returns `None` (no sharing) when
/// it cannot be created or belongs to somebody else: on a shared `/tmp`
/// another user must not be able to feed this process command output.
fn cache_dir() -> Option<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let dir = std::env::temp_dir().join(format!("tmux-agent-sidebar-cache-{uid}"));
    let _ = std::fs::DirBuilder::new().mode(0o700).create(&dir);
    let meta = std::fs::symlink_metadata(&dir).ok()?;
    (meta.is_dir() && meta.uid() == uid && meta.mode() & 0o077 == 0).then_some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn counted<'a>(runs: &'a Cell<u32>, output: &'a str) -> impl FnOnce() -> Option<String> + 'a {
        move || {
            runs.set(runs.get() + 1);
            Some(output.to_string())
        }
    }

    #[test]
    fn second_sidebar_reads_what_the_first_one_ran() {
        let dir = tempfile::tempdir().unwrap();
        let runs = Cell::new(0);
        let now = now_ms();
        let age = Duration::from_secs(5);
        let first = shared_output_in(dir.path(), "ps", age, 0, now, counted(&runs, "one"));
        let second = shared_output_in(dir.path(), "ps", age, 0, now, counted(&runs, "two"));
        assert_eq!(first.as_deref(), Some("one"));
        assert_eq!(second.as_deref(), Some("one"));
        assert_eq!(
            runs.get(),
            1,
            "twelve sidebars must not run ps twelve times"
        );
    }

    #[test]
    fn output_older_than_the_allowed_age_is_run_again() {
        let dir = tempfile::tempdir().unwrap();
        let runs = Cell::new(0);
        let now = now_ms();
        let age = Duration::from_millis(900);
        shared_output_in(dir.path(), "ps", age, 0, now, counted(&runs, "old"));
        let later = shared_output_in(dir.path(), "ps", age, 0, now + 5_000, counted(&runs, "new"));
        assert_eq!(later.as_deref(), Some("new"));
        assert_eq!(runs.get(), 2);
    }

    #[test]
    fn hidden_sidebar_accepts_what_a_visible_one_would_not() {
        let dir = tempfile::tempdir().unwrap();
        let runs = Cell::new(0);
        let now = now_ms();
        shared_output_in(
            dir.path(),
            "ps",
            VISIBLE_MAX_AGE,
            0,
            now,
            counted(&runs, "a"),
        );
        let hidden = shared_output_in(
            dir.path(),
            "ps",
            HIDDEN_MAX_AGE,
            0,
            now + 10_000,
            counted(&runs, "b"),
        );
        assert_eq!(hidden.as_deref(), Some("a"));
        assert_eq!(runs.get(), 1);
    }

    #[test]
    fn output_from_before_an_own_tmux_change_is_not_reused() {
        let dir = tempfile::tempdir().unwrap();
        let runs = Cell::new(0);
        let now = now_ms();
        let age = Duration::from_secs(5);
        shared_output_in(dir.path(), "panes", age, 0, now, counted(&runs, "before"));
        let after = shared_output_in(
            dir.path(),
            "panes",
            age,
            now + 60_000,
            now + 60_001,
            counted(&runs, "after"),
        );
        assert_eq!(after.as_deref(), Some("after"));
        assert_eq!(runs.get(), 2);
    }

    #[test]
    fn different_commands_do_not_share_output() {
        let dir = tempfile::tempdir().unwrap();
        let runs = Cell::new(0);
        let now = now_ms();
        let age = Duration::from_secs(5);
        shared_output_in(dir.path(), "git /a", age, 0, now, counted(&runs, "main"));
        let other = shared_output_in(dir.path(), "git /b", age, 0, now, counted(&runs, "dev"));
        assert_eq!(other.as_deref(), Some("dev"));
    }

    #[test]
    fn failed_run_is_not_shared() {
        let dir = tempfile::tempdir().unwrap();
        let now = now_ms();
        let age = Duration::from_secs(5);
        assert!(shared_output_in(dir.path(), "ps", age, 0, now, || None).is_none());
        let runs = Cell::new(0);
        let next = shared_output_in(dir.path(), "ps", age, 0, now, counted(&runs, "ok"));
        assert_eq!(next.as_deref(), Some("ok"));
        assert_eq!(runs.get(), 1);
    }
}
