//! Now-playing backends.
//!
//! Every backend implements [`MediaSource`]: a non-blocking `poll` returning
//! the last known track plus a transport action. Backends own their worker
//! threads; the UI loop must never wait on a system media API.

use island_core::media::{Media, PlayerAction};

#[cfg(target_os = "macos")]
pub mod macos;
pub mod mock;
#[cfg(windows)]
pub mod windows;

pub trait MediaSource {
    /// Last known track, or `None` when no allowed player is present.
    /// Must be cheap: callers poll at 2 Hz.
    fn poll(&mut self) -> Option<Media>;

    /// Previous / play-pause / next.
    fn action(&mut self, action: PlayerAction);

    /// Short backend name for logs and `--verbose`.
    fn backend(&self) -> &'static str;
}

/// Builds the backend for this platform.
pub fn create(capture_apps: Vec<String>, mock: bool) -> Box<dyn MediaSource> {
    if mock {
        return Box::new(mock::MockMedia::new());
    }
    #[cfg(windows)]
    {
        match windows::WindowsMedia::new(capture_apps.clone()) {
            Ok(m) => return Box::new(m),
            Err(e) => eprintln!("gabes-island: SMTC unavailable ({e}); showing nothing"),
        }
    }
    #[cfg(target_os = "macos")]
    {
        return Box::new(macos::MacMedia::new(capture_apps));
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = capture_apps;
        eprintln!("gabes-island: no native media backend on this platform; using --mock");
        return Box::new(mock::MockMedia::new());
    }
    #[allow(unreachable_code)]
    {
        let _ = capture_apps;
        Box::new(mock::MockMedia::new())
    }
}

/// Whether a backend's own tests can run here (used by `--self-test`).
pub fn backend_name() -> &'static str {
    if cfg!(windows) {
        "smtc"
    } else if cfg!(target_os = "macos") {
        "mediaremote"
    } else {
        "mock"
    }
}
