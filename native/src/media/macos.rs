//! macOS now-playing backend.
//!
//! Two sources, tried in this order:
//!
//! 1. **MediaRemote** (the private framework behind Control Center) via
//!    `dlopen`/`dlsym`. It is the only system-wide "now playing" read API and
//!    gives title, artist, album, elapsed time, duration, artwork bytes and the
//!    owning app's PID. Apple has tightened it over the years (macOS 15.4+
//!    restricts it for non-entitled apps), so failure is expected and handled.
//! 2. **AppleScript** against Spotify and Music. Public, stable, slightly
//!    slower (a few ms per poll) and covers the default `capture_apps=spotify`
//!    even when MediaRemote is blocked.
//!
//! Transport commands use MediaRemote when available and AppleScript otherwise.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use block2::RcBlock;
use island_core::media::{is_capture_allowed, Media, PlayerAction};
use objc2_app_kit::NSRunningApplication;

use super::MediaSource;

// MediaRemote commands (MRMediaRemoteCommand).
const CMD_TOGGLE_PLAY_PAUSE: u32 = 2;
const CMD_NEXT_TRACK: u32 = 4;
const CMD_PREVIOUS_TRACK: u32 = 5;

// CoreFoundation constants used by the raw FFI below.
const CF_UTF8: u32 = 0x0800_0100;
const CF_NUMBER_DOUBLE: c_int = 13;

extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut c_void;
    fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
    fn CFStringGetCString(s: *const c_void, buf: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFNumberGetValue(n: *const c_void, kind: c_int, out: *mut c_void) -> u8;
    fn CFDataGetLength(d: *const c_void) -> isize;
    fn CFDataGetBytePtr(d: *const c_void) -> *const u8;
}

type GetInfoFn = unsafe extern "C" fn(*mut c_void, *mut c_void);
type GetPidFn = unsafe extern "C" fn(*mut c_void, *mut c_void);
type SendCommandFn = unsafe extern "C" fn(u32, *const c_void);

struct MediaRemote {
    handle: *mut c_void,
    get_info: GetInfoFn,
    get_pid: GetPidFn,
    send_command: SendCommandFn,
    keys: Keys,
    queue: *mut c_void,
}

struct Keys {
    title: *const c_void,
    artist: *const c_void,
    album: *const c_void,
    duration: *const c_void,
    elapsed: *const c_void,
    rate: *const c_void,
    artwork: *const c_void,
}

unsafe fn symbol(handle: *mut c_void, name: &str) -> *mut c_void {
    let c = CString::new(name).ok();
    match c {
        Some(c) => libc::dlsym(handle, c.as_ptr()),
        None => std::ptr::null_mut(),
    }
}

unsafe fn cf_key(handle: *mut c_void, name: &str) -> *const c_void {
    let sym = symbol(handle, name);
    if sym.is_null() {
        return std::ptr::null();
    }
    // These are exported as `CFStringRef kMR...`, i.e. a pointer to the ref.
    *(sym as *const *const c_void)
}

impl MediaRemote {
    fn load() -> Option<MediaRemote> {
        unsafe {
            let path =
                CString::new("/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote")
                    .ok()?;
            let handle = libc::dlopen(path.as_ptr(), libc::RTLD_LAZY);
            if handle.is_null() {
                return None;
            }
            let get_info = symbol(handle, "MRMediaRemoteGetNowPlayingInfo");
            let get_pid = symbol(handle, "MRMediaRemoteGetNowPlayingApplicationPID");
            let send_command = symbol(handle, "MRMediaRemoteSendCommand");
            if get_info.is_null() || get_pid.is_null() || send_command.is_null() {
                return None;
            }
            let label = CString::new("gabes-island.mediaremote").ok()?;
            let queue = dispatch_queue_create(label.as_ptr(), std::ptr::null());
            let keys = Keys {
                title: cf_key(handle, "kMRMediaRemoteNowPlayingInfoTitle"),
                artist: cf_key(handle, "kMRMediaRemoteNowPlayingInfoArtist"),
                album: cf_key(handle, "kMRMediaRemoteNowPlayingInfoAlbum"),
                duration: cf_key(handle, "kMRMediaRemoteNowPlayingInfoDuration"),
                elapsed: cf_key(handle, "kMRMediaRemoteNowPlayingInfoElapsedTime"),
                rate: cf_key(handle, "kMRMediaRemoteNowPlayingInfoPlaybackRate"),
                artwork: cf_key(handle, "kMRMediaRemoteNowPlayingInfoArtworkData"),
            };
            Some(MediaRemote {
                handle,
                get_info: std::mem::transmute::<*mut c_void, GetInfoFn>(get_info),
                get_pid: std::mem::transmute::<*mut c_void, GetPidFn>(get_pid),
                send_command: std::mem::transmute::<*mut c_void, SendCommandFn>(send_command),
                keys,
                queue,
            })
        }
    }

    /// Synchronous snapshot: the block runs on our serial queue, we wait 400 ms.
    fn snapshot(&self) -> Option<NowPlaying> {
        let (tx, rx) = mpsc::channel::<Option<NowPlaying>>();
        let keys = RawKeys {
            title: self.keys.title,
            artist: self.keys.artist,
            album: self.keys.album,
            duration: self.keys.duration,
            elapsed: self.keys.elapsed,
            rate: self.keys.rate,
            artwork: self.keys.artwork,
        };
        let block = RcBlock::new(move |info: *const c_void| {
            let parsed = if info.is_null() {
                None
            } else {
                unsafe { read_info(info, &keys) }
            };
            let _ = tx.send(parsed);
        });
        unsafe {
            (self.get_info)(self.queue, &*block as *const _ as *mut c_void);
        }
        let mut np = rx.recv_timeout(Duration::from_millis(400)).ok().flatten()?;
        np.pid = self.pid();
        Some(np)
    }

    fn pid(&self) -> i32 {
        let (tx, rx) = mpsc::channel::<i32>();
        let block = RcBlock::new(move |pid: c_int| {
            let _ = tx.send(pid);
        });
        unsafe {
            (self.get_pid)(self.queue, &*block as *const _ as *mut c_void);
        }
        rx.recv_timeout(Duration::from_millis(300)).unwrap_or(0)
    }

    fn send(&self, command: u32) {
        unsafe {
            (self.send_command)(command, std::ptr::null());
        }
    }
}

impl Drop for MediaRemote {
    fn drop(&mut self) {
        unsafe {
            if !self.handle.is_null() {
                libc::dlclose(self.handle);
            }
        }
    }
}

#[derive(Clone, Copy)]
struct RawKeys {
    title: *const c_void,
    artist: *const c_void,
    album: *const c_void,
    duration: *const c_void,
    elapsed: *const c_void,
    rate: *const c_void,
    artwork: *const c_void,
}

unsafe impl Send for RawKeys {}

struct NowPlaying {
    title: String,
    artist: String,
    album: String,
    duration_s: f64,
    elapsed_s: f64,
    rate: f64,
    artwork: Option<Vec<u8>>,
    pid: i32,
}

unsafe fn read_info(dict: *const c_void, keys: &RawKeys) -> Option<NowPlaying> {
    let s = |key: *const c_void| -> String {
        if key.is_null() {
            return String::new();
        }
        let value = CFDictionaryGetValue(dict, key);
        if value.is_null() {
            return String::new();
        }
        let mut buf = vec![0 as c_char; 1024];
        let ok = CFStringGetCString(value, buf.as_mut_ptr(), buf.len() as isize, CF_UTF8);
        if ok == 0 {
            return String::new();
        }
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    };
    let n = |key: *const c_void| -> f64 {
        if key.is_null() {
            return 0.0;
        }
        let value = CFDictionaryGetValue(dict, key);
        if value.is_null() {
            return 0.0;
        }
        let mut out = 0.0f64;
        let ok = CFNumberGetValue(value, CF_NUMBER_DOUBLE, &mut out as *mut f64 as *mut c_void);
        if ok == 0 {
            0.0
        } else {
            out
        }
    };
    let title = s(keys.title);
    if title.is_empty() {
        return None;
    }
    let artwork = {
        let data = CFDictionaryGetValue(dict, keys.artwork);
        if data.is_null() {
            None
        } else {
            let len = CFDataGetLength(data);
            if len <= 0 || len > 16 * 1024 * 1024 {
                None
            } else {
                let ptr = CFDataGetBytePtr(data);
                if ptr.is_null() {
                    None
                } else {
                    Some(std::slice::from_raw_parts(ptr, len as usize).to_vec())
                }
            }
        }
    };
    Some(NowPlaying {
        title,
        artist: s(keys.artist),
        album: s(keys.album),
        duration_s: n(keys.duration),
        elapsed_s: n(keys.elapsed),
        rate: n(keys.rate),
        artwork,
        pid: 0,
    })
}

fn bundle_id(pid: i32) -> String {
    if pid <= 0 {
        return String::new();
    }
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid);
    app.and_then(|a| a.bundleIdentifier())
        .map(|b| b.to_string())
        .unwrap_or_default()
}

pub struct MacMedia {
    latest: Arc<Mutex<Option<Media>>>,
    actions: Sender<PlayerAction>,
}

impl MacMedia {
    pub fn new(capture_apps: Vec<String>) -> MacMedia {
        let latest: Arc<Mutex<Option<Media>>> = Arc::new(Mutex::new(None));
        let shared = latest.clone();
        let (action_tx, action_rx) = mpsc::channel::<PlayerAction>();
        thread::Builder::new()
            .name("mediaremote".into())
            .spawn(move || worker(shared, action_rx, capture_apps))
            .ok();
        MacMedia {
            latest,
            actions: action_tx,
        }
    }
}

impl MediaSource for MacMedia {
    fn poll(&mut self) -> Option<Media> {
        self.latest.lock().ok().and_then(|m| m.clone())
    }

    fn action(&mut self, action: PlayerAction) {
        let _ = self.actions.send(action);
    }

    fn backend(&self) -> &'static str {
        "mediaremote"
    }
}

fn worker(latest: Arc<Mutex<Option<Media>>>, actions: Receiver<PlayerAction>, apps: Vec<String>) {
    let mr = MediaRemote::load();
    loop {
        match actions.recv_timeout(Duration::from_millis(500)) {
            Ok(action) => {
                match mr.as_ref() {
                    Some(m) => m.send(match action {
                        PlayerAction::PlayPause => CMD_TOGGLE_PLAY_PAUSE,
                        PlayerAction::Next => CMD_NEXT_TRACK,
                        PlayerAction::Previous => CMD_PREVIOUS_TRACK,
                    }),
                    None => applescript_action(&apps, action),
                }
                // fall through to refresh right away
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }

        let mut media = mr.as_ref().and_then(|m| from_mediaremote(m, &apps));
        if media.is_none() {
            media = applescript_poll(&apps);
        }
        if let Ok(mut slot) = latest.lock() {
            *slot = media;
        }
    }
}

fn from_mediaremote(mr: &MediaRemote, apps: &[String]) -> Option<Media> {
    let np = mr.snapshot()?;
    let bundle = bundle_id(np.pid);
    let app_name = bundle.rsplit('.').next().unwrap_or(&bundle).to_string();
    if !is_capture_allowed(apps, &bundle, &app_name) {
        return None;
    }
    let playing = if np.rate > 0.01 {
        1
    } else if np.duration_s > 0.0 {
        2
    } else {
        0
    };
    Some(Media {
        playing,
        player: bundle,
        app_name,
        title: np.title,
        artist: np.artist,
        album: np.album,
        art: String::new(),
        cover: np.artwork,
        length_us: (np.duration_s * 1_000_000.0) as i64,
        pos_us: (np.elapsed_s * 1_000_000.0) as i64,
    })
}

/// Which AppleScript apps the allowlist asks for (empty allowlist: both).
fn script_apps(apps: &[String]) -> Vec<&'static str> {
    let allow_all = apps.is_empty();
    let mut out = Vec::new();
    if allow_all || apps.iter().any(|a| a.contains("spotify")) {
        out.push("Spotify");
    }
    if allow_all || apps.iter().any(|a| a.contains("music")) {
        out.push("Music");
    }
    out
}

fn run_osascript(script: &str) -> Option<String> {
    let out = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn applescript_poll(apps: &[String]) -> Option<Media> {
    for app in script_apps(apps) {
        // Spotify reports duration in milliseconds, Music in seconds.
        let (bundle, duration_expr) = match app {
            "Spotify" => ("com.spotify.client", "duration of current track / 1000"),
            _ => ("com.apple.Music", "duration of current track"),
        };
        let script = format!(
            r#"tell application "{app}"
if it is running then
  set st to player state as string
  if st is "playing" or st is "paused" then
    return st & tab & (name of current track) & tab & (artist of current track) & tab & (album of current track) & tab & (({duration_expr}) as string) & tab & ((player position) as string)
  end if
end if
return """#
        );
        let Some(line) = run_osascript(&script) else {
            continue;
        };
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 6 {
            continue;
        }
        let playing = if fields[0].eq_ignore_ascii_case("playing") {
            1
        } else {
            2
        };
        let durations_s: f64 = fields[4].parse().unwrap_or(0.0);
        let pos_s: f64 = fields[5].parse().unwrap_or(0.0);
        return Some(Media {
            playing,
            player: bundle.to_string(),
            app_name: app.to_string(),
            title: fields[1].to_string(),
            artist: fields[2].to_string(),
            album: fields[3].to_string(),
            art: String::new(),
            cover: None,
            length_us: (durations_s * 1_000_000.0) as i64,
            pos_us: (pos_s * 1_000_000.0) as i64,
        });
    }
    None
}

fn applescript_action(apps: &[String], action: PlayerAction) {
    let verb = match action {
        PlayerAction::PlayPause => "playpause",
        PlayerAction::Next => "next track",
        PlayerAction::Previous => "previous track",
    };
    for app in script_apps(apps) {
        let script = format!("tell application \"{app}\" to if it is running then {verb}");
        let _ = run_osascript(&script);
        if app == "Spotify" {
            break;
        }
    }
}
