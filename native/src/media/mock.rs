//! A fake Spotify session for development, previews and the Linux test build.
//!
//! It behaves like a real player: a 3:40 track, position advancing in real
//! time, transport actions, and a small playlist for next/previous.

use island_core::media::{Media, PlayerAction};

use super::MediaSource;

pub struct MockMedia {
    tracks: Vec<(String, String)>,
    idx: usize,
    playing: u8,
    /// Position at the moment playback was last started/seeked, in µs.
    base_us: i64,
    /// Monotonic start point (ms) for the current run.
    started_ms: u128,
    length_us: i64,
    paused_at: u128,
    start: std::time::Instant,
}

impl Default for MockMedia {
    fn default() -> Self {
        Self::new()
    }
}

impl MockMedia {
    pub fn new() -> MockMedia {
        MockMedia::with_tracks(vec![
            ("Get Lucky".into(), "Daft Punk, Pharrell Williams".into()),
            ("Midnight City".into(), "M83".into()),
            ("夜に駆ける".into(), "YOASOBI".into()),
            ("Spokojnie".into(), "Myslovitz".into()),
        ])
    }

    pub fn with_tracks(tracks: Vec<(String, String)>) -> MockMedia {
        MockMedia {
            tracks: if tracks.is_empty() {
                vec![("Mock Track".into(), "Mock Artist".into())]
            } else {
                tracks
            },
            idx: 0,
            playing: 1,
            base_us: 0,
            started_ms: 0,
            length_us: 220_000_000,
            paused_at: 0,
            start: std::time::Instant::now(),
        }
    }

    /// Jumps the fake clock; useful for deterministic screenshots/tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn seek(&mut self, us: i64) {
        self.base_us = us.clamp(0, self.length_us);
        self.started_ms = self.now();
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_playing(&mut self, playing: u8) {
        self.playing = playing;
        self.started_ms = self.now();
    }

    fn now(&self) -> u128 {
        self.start.elapsed().as_millis()
    }

    pub fn position_us(&mut self) -> i64 {
        let mut pos = self.base_us;
        if self.playing == 1 {
            pos += (self.now().saturating_sub(self.started_ms)) as i64 * 1000;
        }
        let _ = self.paused_at;
        pos.min(self.length_us).max(0)
    }
}

impl MediaSource for MockMedia {
    fn poll(&mut self) -> Option<Media> {
        let (title, artist) = self.tracks[self.idx].clone();
        let pos = self.position_us();
        Some(Media {
            playing: self.playing,
            player: "com.spotify.client".into(),
            app_name: "Spotify".into(),
            title,
            artist,
            album: "Random Access Memories".into(),
            art: String::new(),
            cover: None,
            length_us: self.length_us,
            pos_us: pos,
        })
    }

    fn action(&mut self, action: PlayerAction) {
        match action {
            PlayerAction::PlayPause => {
                self.playing = if self.playing == 1 { 2 } else { 1 };
                self.started_ms = self.now();
            }
            PlayerAction::Next => {
                self.idx = (self.idx + 1) % self.tracks.len();
                self.base_us = 0;
                self.started_ms = self.now();
            }
            PlayerAction::Previous => {
                self.idx = if self.idx == 0 {
                    self.tracks.len() - 1
                } else {
                    self.idx - 1
                };
                self.base_us = 0;
                self.started_ms = self.now();
            }
        }
    }

    fn backend(&self) -> &'static str {
        "mock"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_advances_while_playing() {
        let mut m = MockMedia::new();
        let a = m.poll().unwrap();
        assert_eq!(a.playing, 1);
        assert_eq!(a.player, "com.spotify.client");
        std::thread::sleep(std::time::Duration::from_millis(30));
        let b = m.poll().unwrap();
        assert!(b.pos_us >= a.pos_us);
        m.set_playing(2);
        let c = m.poll().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(m.poll().unwrap().pos_us, c.pos_us);
    }

    #[test]
    fn transport_actions_change_the_track() {
        let mut m = MockMedia::new();
        let first = m.poll().unwrap().title;
        m.action(PlayerAction::Next);
        assert_ne!(m.poll().unwrap().title, first);
        m.action(PlayerAction::Previous);
        assert_eq!(m.poll().unwrap().title, first);
        m.action(PlayerAction::PlayPause);
        assert_eq!(m.poll().unwrap().playing, 2);
        m.action(PlayerAction::PlayPause);
        assert_eq!(m.poll().unwrap().playing, 1);
    }

    #[test]
    fn position_is_clamped() {
        let mut m = MockMedia::new();
        m.seek(-50);
        assert_eq!(m.position_us(), 0);
        m.seek(999_000_000);
        assert!(m.position_us() <= m.length_us);
    }
}
