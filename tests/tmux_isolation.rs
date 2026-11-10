//! Fork-local guard: `cargo test` must never reach the live tmux server.
//! `.cargo/config.toml` points TMUX_TMPDIR at a private directory; when that
//! directory does not exist, tmux silently falls back to the default socket
//! and the tests rewrite the owner's real sidebar options (filter, hidden
//! flag) and kill real sidebar panes.

#[test]
fn tmux_tmpdir_for_tests_points_at_an_existing_private_dir() {
    let dir = std::env::var("TMUX_TMPDIR").expect("TMUX_TMPDIR set by .cargo/config.toml");
    let path = std::path::Path::new(&dir);
    assert!(
        path.is_dir(),
        "TMUX_TMPDIR {dir} does not exist, tmux would use the live server"
    );
    assert!(path.is_absolute(), "TMUX_TMPDIR {dir} must be absolute");
    assert_eq!(std::env::var("TMUX").as_deref(), Ok(""));
}

#[test]
fn tmux_client_in_tests_finds_no_server() {
    let out = std::process::Command::new("tmux")
        .args(["display-message", "-p", "#{session_name}"])
        .output();
    let Ok(out) = out else { return }; // no tmux binary: nothing to reach
    assert!(
        !out.status.success(),
        "a tmux client in tests reached a live server: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}
