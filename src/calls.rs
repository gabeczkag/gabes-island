//! Messenger call detection for the X11 app.
//!
//! Messenger has no call API on the web, so detection is evidence-based:
//!
//! 1. **Window titles** (`_NET_WM_NAME`/`WM_NAME` of every managed window):
//!    a window whose title belongs to Messenger tells us a conversation is
//!    open, and titles such as "Połączenie przychodzące | Messenger" or
//!    "Wideorozmowa | Messenger" tell us it is a call (and whether it rings).
//! 2. **PulseAudio capture streams** (`pactl list source-outputs`): a browser
//!    capturing audio names the stream after the call ("Rozmowa grupowa"),
//!    which both confirms the call and provides the conversation name.
//! 3. **Camera**: a browser process holding `/dev/video*` means the video
//!    call has the camera on.
//!
//! A call is only reported when a Messenger window exists — audio from some
//! other browser tab (Meet, WhatsApp) must not raise the island.
//!
//! This app cannot answer a web call: there is no API for it, and clicking
//! through the browser UI is not reliable. The banner is therefore
//! informational on Linux; backends that can control a desktop client mark the
//! call `controllable` and the island shows Accept/Decline.

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_uchar, c_ulong, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use island_core::call::{self, Call, CallEvidence, CallSession};

/// Re-exported for the `--debug-calls` report.
pub use island_core::call::looks_like_messenger;
use x11::xlib;

const POLL_MS: u128 = 1_000;
const EVIDENCE_MS: u128 = 2_000;

#[derive(Default, Clone)]
struct AudioState {
    /// `media.name` of browser capture streams.
    names: Vec<String>,
    /// A browser holds a camera device.
    camera: bool,
    /// Best-effort default sink name (unused, kept for logging/debug).
    sink: String,
}

pub struct Detector {
    session: CallSession,
    audio: Arc<Mutex<AudioState>>,
    probe: Arc<AtomicBool>,
    started: bool,
    last_poll: u128,
    last_evidence: u128,
    last_scan: Vec<String>,
    /// Last composition, so a silent poll does not immediately clear a call.
    last_evidence_at: u128,
}

impl Detector {
    pub fn new() -> Detector {
        Detector {
            session: CallSession::new(),
            audio: Arc::new(Mutex::new(AudioState::default())),
            probe: Arc::new(AtomicBool::new(false)),
            started: false,
            last_poll: 0,
            last_evidence: u128::MAX,
            last_scan: Vec::new(),
            last_evidence_at: 0,
        }
    }

    fn start_worker(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        let audio = self.audio.clone();
        let probe = self.probe.clone();
        thread::spawn(move || loop {
            if !probe.load(Ordering::Relaxed) {
                // No Messenger window on screen: keep the audio and camera
                // probes idle instead of spawning pactl every two seconds.
                thread::sleep(Duration::from_millis(EVIDENCE_MS as u64));
                continue;
            }
            let names = pulse_capture_names();
            let camera = browser_camera_in_use();
            let sink = default_sink();
            if let Ok(mut a) = audio.lock() {
                a.names = names;
                a.camera = camera;
                a.sink = sink;
            }
            thread::sleep(Duration::from_millis(EVIDENCE_MS as u64));
        });
    }

    /// Polls (at most once a second) and returns the call to display.
    ///
    /// Must be called from the X11 thread: it queries window titles.
    pub fn poll(&mut self, now: u128, dpy: *mut xlib::Display, root: xlib::Window) -> Option<Call> {
        self.start_worker();
        if now.saturating_sub(self.last_poll) < POLL_MS && self.last_evidence != u128::MAX {
            return self.session.current().cloned();
        }
        self.last_poll = now;

        let titles = unsafe { window_titles(dpy, root) };
        let messenger: Vec<String> = titles
            .iter()
            .filter(|t| call::looks_like_messenger(t))
            .cloned()
            .collect();
        self.last_scan = messenger.clone();
        self.probe.store(
            !messenger.is_empty() || self.session.current().is_some(),
            Ordering::Relaxed,
        );

        let evidence = if messenger.is_empty() {
            None
        } else {
            let audio = self.audio.lock().ok().map(|a| a.clone()).unwrap_or_default();
            let media = audio
                .names
                .iter()
                .find(|n| call::peer_from_media_name(n).is_some());
            let title = messenger
                .iter()
                .find(|t| {
                    call::parse_messenger_title(t)
                        .map(|p| p.call)
                        .unwrap_or(false)
                })
                .or_else(|| messenger.first());
            call::compose(
                title.map(|s| s.as_str()),
                media.map(|s| s.as_str()),
                audio.camera,
                false,
                "messenger-web",
            )
        };

        match evidence {
            Some(ev) => {
                self.last_evidence_at = now;
                self.session.observe(Some(ev), now)
            }
            None => {
                // Evidence vanishes when the user switches tabs or the call
                // drops; the session's hangover smooths that over.
                self.last_evidence_at = now;
                self.session.observe(None, now)
            }
        }
    }

    pub fn session(&self) -> &CallSession {
        &self.session
    }

    /// Last seen window titles (for `--debug-calls`).
    pub fn last_titles(&self) -> &[String] {
        &self.last_scan
    }
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

/// A call view with a backend that cannot control the player.
pub fn evidence_from_parts(title: Option<&str>, media: Option<&str>, camera: bool) -> Option<CallEvidence> {
    call::compose(title, media, camera, false, "messenger-web")
}

// ------------------------------------------------------------------ X11

unsafe fn intern(dpy: *mut xlib::Display, name: &str) -> xlib::Atom {
    let c = CString::new(name).unwrap_or_default();
    xlib::XInternAtom(dpy, c.as_ptr(), xlib::False)
}

/// Reads a property as raw bytes; `None` when it is missing or empty.
unsafe fn get_property(
    dpy: *mut xlib::Display,
    win: xlib::Window,
    prop: xlib::Atom,
    kind: xlib::Atom,
) -> Option<Vec<u8>> {
    if prop == 0 {
        return None;
    }
    let mut actual_type: xlib::Atom = 0;
    let mut actual_format: c_int = 0;
    let mut nitems: c_ulong = 0;
    let mut bytes_after: c_ulong = 0;
    let mut data: *mut c_uchar = std::ptr::null_mut();
    let status = xlib::XGetWindowProperty(
        dpy,
        win,
        prop,
        0,
        4096,
        xlib::False,
        kind,
        &mut actual_type,
        &mut actual_format,
        &mut nitems,
        &mut bytes_after,
        &mut data,
    );
    if status != xlib::Success as c_int || data.is_null() {
        return None;
    }
    // format 32 returns one C `long` per item (8 bytes on LP64), not 4.
    let per_item = if actual_format == 32 {
        std::mem::size_of::<c_ulong>()
    } else {
        1
    };
    let len = nitems as usize * per_item;
    let bytes = std::slice::from_raw_parts(data as *const u8, len).to_vec();
    xlib::XFree(data as *mut c_void);
    Some(bytes)
}

/// Titles of all managed top-level windows.
pub unsafe fn window_titles(dpy: *mut xlib::Display, root: xlib::Window) -> Vec<String> {
    let net_client_list = intern(dpy, "_NET_CLIENT_LIST");
    let net_wm_name = intern(dpy, "_NET_WM_NAME");
    let utf8 = intern(dpy, "UTF8_STRING");

    let mut windows: Vec<xlib::Window> = Vec::new();
    if let Some(raw) = get_property(dpy, root, net_client_list, xlib::XA_WINDOW) {
        for chunk in raw.chunks_exact(std::mem::size_of::<c_ulong>()) {
            let mut bytes = [0u8; std::mem::size_of::<c_ulong>()];
            bytes.copy_from_slice(chunk);
            let w = c_ulong::from_ne_bytes(bytes);
            if w != 0 {
                windows.push(w as xlib::Window);
            }
        }
    }
    if windows.is_empty() {
        // No EWMH: walk the root's children.
        let mut parent: xlib::Window = 0;
        let mut children: *mut xlib::Window = std::ptr::null_mut();
        let mut count: u32 = 0;
        if xlib::XQueryTree(dpy, root, &mut parent, &mut parent, &mut children, &mut count) != 0
            && !children.is_null()
        {
            for i in 0..count as isize {
                let w = *children.offset(i);
                if w != 0 {
                    windows.push(w);
                }
            }
            xlib::XFree(children as *mut c_void);
        }
    }

    let mut out = Vec::with_capacity(windows.len());
    for w in windows {
        let mut title: Option<String> = None;
        if let Some(raw) = get_property(dpy, w, net_wm_name, utf8) {
            let s = String::from_utf8_lossy(&raw).trim_matches('\0').trim().to_string();
            if !s.is_empty() {
                title = Some(s);
            }
        }
        if title.is_none() {
            let mut name: *mut c_char = std::ptr::null_mut();
            if xlib::XFetchName(dpy, w, &mut name) != 0 && !name.is_null() {
                let s = std::ffi::CStr::from_ptr(name).to_string_lossy().trim().to_string();
                xlib::XFree(name as *mut c_void);
                if !s.is_empty() {
                    title = Some(s);
                }
            }
        }
        if let Some(t) = title {
            out.push(t);
        }
    }
    out
}

// ------------------------------------------------------------- PulseAudio

/// `media.name` of every browser capture stream on the default server.
pub fn pulse_capture_names() -> Vec<String> {
    let out = std::process::Command::new("pactl")
        .args(["list", "source-outputs"])
        .output();
    let Ok(out) = out else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_source_outputs(&text)
}

/// Parses `pactl list source-outputs` output.
pub fn parse_source_outputs(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut app = String::new();
    let mut media = String::new();
    let mut is_browser = false;
    let flush = |app: &str, media: &str, browser: bool, out: &mut Vec<String>| {
        if browser && !media.trim().is_empty() && !app.trim().is_empty() {
            out.push(media.trim().to_string());
        }
    };
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("Source Output #") {
            flush(&app, &media, is_browser, &mut names);
            app.clear();
            media.clear();
            is_browser = false;
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim().trim_matches('"');
            match k.trim() {
                "application.name" => {
                    app = v.to_string();
                    is_browser = is_browser_app(v);
                }
                "media.name" => media = v.to_string(),
                _ => {}
            }
        }
    }
    flush(&app, &media, is_browser, &mut names);
    names
}

fn is_browser_app(name: &str) -> bool {
    let n = name.to_lowercase();
    ["firefox", "chrome", "chromium", "brave", "edge", "vivaldi", "opera"]
        .iter()
        .any(|b| n.contains(b))
}

fn default_sink() -> String {
    std::process::Command::new("pactl")
        .arg("get-default-sink")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

// ----------------------------------------------------------------- camera

/// True when a browser process has a `/dev/video*` device open.
pub fn browser_camera_in_use() -> bool {
    let Ok(proc) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in proc.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        if !is_browser_comm(&comm) {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if let Ok(target) = std::fs::read_link(fd.path()) {
                if target.to_string_lossy().starts_with("/dev/video") {
                    return true;
                }
            }
        }
    }
    false
}

fn is_browser_comm(comm: &str) -> bool {
    ["firefox", "chrome", "chromium", "brave", "msedge", "vivaldi", "opera"]
        .iter()
        .any(|b| comm.starts_with(b) || comm.contains(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
Source Output #12
	Driver: PipeWire
	Owner Module: 4294967295
	Client: 41
	application.name = "Firefox"
	application.process.binary = "firefox-bin"
	media.name = "Rozmowa grupowa"
	state: RUNNING

Source Output #13
	application.name = "Firefox"
	media.name = "AudioCallbackDriver"
	state: RUNNING
"#;

    #[test]
    fn parses_browser_capture_streams() {
        let names = parse_source_outputs(SAMPLE);
        assert_eq!(names, vec!["Rozmowa grupowa".to_string(), "AudioCallbackDriver".to_string()]);
    }

    #[test]
    fn ignores_non_browser_streams() {
        let sample = r#"
Source Output #1
	application.name = "Audacity"
	media.name = "Rozmowa"
"#;
        assert!(parse_source_outputs(sample).is_empty());
    }

    #[test]
    fn browser_comm_matching() {
        assert!(is_browser_comm("firefox"));
        assert!(is_browser_comm("firefox-bin"));
        assert!(is_browser_comm("chrome"));
        assert!(!is_browser_comm("gabes-island"));
        assert!(!is_browser_comm("pulseaudio"));
    }

    #[test]
    fn evidence_requires_a_messenger_title() {
        assert!(evidence_from_parts(None, Some("Rozmowa grupowa"), true).is_none());
        assert!(evidence_from_parts(Some("Messenger"), None, false).is_none());
        let ev = evidence_from_parts(
            Some("Rozmowa grupowa | Messenger"),
            Some("Rozmowa grupowa"),
            true,
        )
        .unwrap();
        assert!(ev.kind == island_core::call::CallKind::Video);
    }

    #[test]
    fn detector_is_idle_without_a_display_scan() {
        // A fresh detector with no evidence must not invent a call.
        let mut d = Detector::new();
        d.last_evidence_at = 0;
        assert!(d.session().current().is_none());
    }
}
