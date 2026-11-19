//! Windows now-playing backend: the System Media Transport Controls (SMTC).
//!
//! SMTC is what the volume flyout reads: Spotify, browsers, VLC and most
//! modern players publish title/artist/album/artwork/position there and accept
//! transport commands. Everything runs on a worker thread with its own COM
//! apartment, so the UI loop never blocks on a WinRT call.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use island_core::media::{is_capture_allowed, Media, PlayerAction};
use windows::core::HSTRING;
use windows::Foundation::{DateTime, TimeSpan};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession, GlobalSystemMediaTransportControlsSessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use super::MediaSource;

/// 100 ns ticks between 1601-01-01 (Win32 epoch) and 1970-01-01 (Unix epoch).
const EPOCH_DIFF_100NS: u64 = 116_444_736_000_000_000;

type Shared = Arc<Mutex<Option<Media>>>;

pub struct WindowsMedia {
    latest: Shared,
    actions: Sender<PlayerAction>,
}

impl WindowsMedia {
    /// Starts the worker and waits (briefly) for the first successful SMTC
    /// handshake so a missing/broken media stack is reported, not hidden.
    pub fn new(capture_apps: Vec<String>) -> Result<WindowsMedia, String> {
        let latest: Shared = Arc::new(Mutex::new(None));
        let shared = latest.clone();
        let (action_tx, action_rx) = mpsc::channel::<PlayerAction>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        thread::Builder::new()
            .name("smtc".into())
            .spawn(move || worker(shared, action_rx, capture_apps, ready_tx))
            .map_err(|e| e.to_string())?;

        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(WindowsMedia {
                latest,
                actions: action_tx,
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("SMTC did not answer within 3 s".into()),
        }
    }
}

impl MediaSource for WindowsMedia {
    fn poll(&mut self) -> Option<Media> {
        self.latest.lock().ok().and_then(|m| m.clone())
    }

    fn action(&mut self, action: PlayerAction) {
        let _ = self.actions.send(action);
    }

    fn backend(&self) -> &'static str {
        "smtc"
    }
}

fn worker(
    latest: Shared,
    actions: Receiver<PlayerAction>,
    capture_apps: Vec<String>,
    ready: Sender<Result<(), String>>,
) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let manager = match GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
        .and_then(|op| op.join())
    {
        Ok(m) => m,
        Err(e) => {
            let _ = ready.send(Err(format!("SMTC manager: {e}")));
            return;
        }
    };
    let _ = ready.send(Ok(()));

    loop {
        match actions.recv_timeout(Duration::from_millis(500)) {
            Ok(action) => {
                if let Some(session) = current_session(&manager, &capture_apps) {
                    let _ = match action {
                        PlayerAction::PlayPause => session.TryTogglePlayPauseAsync(),
                        PlayerAction::Next => session.TrySkipNextAsync(),
                        PlayerAction::Previous => session.TrySkipPreviousAsync(),
                    };
                }
                // poll right away so the UI reflects the action immediately
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        let media = current_session(&manager, &capture_apps).and_then(|s| read_session(&s));
        if let Ok(mut slot) = latest.lock() {
            *slot = media;
        }
    }
}

fn allowed(capture_apps: &[String], aumid: &str) -> bool {
    is_capture_allowed(capture_apps, aumid, &pretty_name(aumid))
}

/// Prefers a session that is playing and allowed, then the system's current
/// session, then any allowed session.
fn current_session(
    manager: &GlobalSystemMediaTransportControlsSessionManager,
    capture_apps: &[String],
) -> Option<GlobalSystemMediaTransportControlsSession> {
    let sessions: Vec<GlobalSystemMediaTransportControlsSession> = manager
        .GetSessions()
        .ok()
        .map(|v| {
            (0..v.Size().unwrap_or(0))
                .filter_map(|i| v.GetAt(i).ok())
                .collect()
        })
        .unwrap_or_default();

    let mut any = None;
    for s in &sessions {
        let aumid = s
            .SourceAppUserModelId()
            .map(|h| h.to_string())
            .unwrap_or_default();
        if !allowed(capture_apps, &aumid) {
            continue;
        }
        let playing = s
            .GetPlaybackInfo()
            .and_then(|p| p.PlaybackStatus())
            .map(|st| st == PlaybackStatus::Playing)
            .unwrap_or(false);
        if playing {
            return Some(s.clone());
        }
        if any.is_none() {
            any = Some(s.clone());
        }
    }
    if let Ok(current) = manager.GetCurrentSession() {
        let aumid = current
            .SourceAppUserModelId()
            .map(|h| h.to_string())
            .unwrap_or_default();
        if allowed(capture_apps, &aumid) {
            return Some(current);
        }
    }
    any
}

fn read_session(session: &GlobalSystemMediaTransportControlsSession) -> Option<Media> {
    let props = session
        .TryGetMediaPropertiesAsync()
        .and_then(|op| op.join())
        .ok()?;
    let timeline = session.GetTimelineProperties().ok();
    let playback = session.GetPlaybackInfo().ok();

    let player = session
        .SourceAppUserModelId()
        .map(|h| h.to_string())
        .unwrap_or_default();
    let status = playback
        .as_ref()
        .and_then(|p| p.PlaybackStatus().ok())
        .unwrap_or(PlaybackStatus::Closed);
    let playing = match status {
        PlaybackStatus::Playing => 1,
        PlaybackStatus::Paused | PlaybackStatus::Changing => 2,
        _ => 0,
    };
    let rate = playback
        .as_ref()
        .and_then(|p| p.PlaybackRate().ok())
        .and_then(|r| r.Value().ok())
        .unwrap_or(1.0);

    let (mut pos_us, length_us) = match &timeline {
        Some(t) => {
            let position = t.Position().map(|p: TimeSpan| p.Duration).unwrap_or(0) / 10;
            let end = t.EndTime().map(|p: TimeSpan| p.Duration).unwrap_or(0) / 10;
            let mut extra = 0i64;
            if playing == 1 {
                if let Ok(updated) = t.LastUpdatedTime() {
                    let elapsed = now_100ns().saturating_sub(updated.UniversalTime.max(0) as u64);
                    extra = (elapsed / 10) as i64;
                    extra = (extra as f64 * rate) as i64;
                }
            }
            (position + extra, end)
        }
        None => (0, 0),
    };
    pos_us = pos_us.max(0);
    if length_us > 0 {
        pos_us = pos_us.min(length_us);
    }

    let cover = props
        .Thumbnail()
        .ok()
        .and_then(|thumb: IRandomAccessStreamReference| read_stream(&thumb));

    Some(Media {
        playing,
        app_name: pretty_name(&player),
        player,
        title: props.Title().map(|s| s.to_string()).unwrap_or_default(),
        artist: props.Artist().map(|s| s.to_string()).unwrap_or_default(),
        album: props
            .AlbumTitle()
            .map(|s| s.to_string())
            .unwrap_or_default(),
        art: String::new(),
        cover,
        length_us,
        pos_us,
    })
}

fn read_stream(reference: &IRandomAccessStreamReference) -> Option<Vec<u8>> {
    let stream = reference.OpenReadAsync().ok()?.join().ok()?;
    let size = stream.Size().ok()? as u32;
    if size == 0 || size > 16 * 1024 * 1024 {
        return None;
    }
    let reader = DataReader::CreateDataReader(&stream).ok()?;
    reader.LoadAsync(size).ok()?.join().ok()?;
    let mut buf = vec![0u8; size as usize];
    reader.ReadBytes(&mut buf).ok()?;
    reader.Close().ok()?;
    Some(buf)
}

fn now_100ns() -> u64 {
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    EPOCH_DIFF_100NS + unix / 100
}

/// `Spotify.exe` / `SpotifyAB.SpotifyMusic_x!Spotify` -> `Spotify`.
fn pretty_name(aumid: &str) -> String {
    let base = aumid.split('!').next().unwrap_or(aumid);
    let base = base.split('_').next().unwrap_or(base);
    let base = base.trim_end_matches(".exe");
    base.rsplit('.').next().unwrap_or(base).to_string()
}

#[allow(dead_code)]
fn _assert_types(_: HSTRING, _: DateTime) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aumids_become_readable_names() {
        assert_eq!(pretty_name("Spotify.exe"), "Spotify");
        assert_eq!(
            pretty_name("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"),
            "SpotifyMusic"
        );
        assert_eq!(pretty_name("firefox.exe"), "firefox");
        assert_eq!(pretty_name(""), "");
    }

    #[test]
    fn capture_filter_is_applied_to_sessions() {
        let apps = island_core::media::parse_capture_apps("spotify");
        assert!(allowed(&apps, "Spotify.exe"));
        assert!(!allowed(&apps, "firefox.exe"));
        let all = island_core::media::parse_capture_apps("");
        assert!(allowed(&all, "firefox.exe"));
    }
}
