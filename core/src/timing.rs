//! The lyric cursor, playback clock and drift compensation.
//!
//! The Linux app keeps an invisible in-memory timeline: a cursor walks the
//! word list exactly once, seeking rewinds or catches up, and a small drift
//! guard nudges an offset when the player's reported position disagrees with
//! where the words actually are.  All of that is pure math, so it lives here
//! and is exercised by the unit tests below.

/// Cursor over a sorted `(ms, word)` list.
#[derive(Clone, Debug, Default)]
pub struct Timeline {
    pub words: Vec<(i64, String)>,
    pub cursor: usize,
}

impl Timeline {
    pub fn new(words: Vec<(i64, String)>) -> Timeline {
        Timeline { words, cursor: 0 }
    }

    pub fn reset(&mut self, words: Vec<(i64, String)>) {
        self.words = words;
        self.cursor = 0;
    }

    pub fn clear(&mut self) {
        self.words.clear();
        self.cursor = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Index of the last word whose time is `<= pos`.
    pub fn index_at(&self, pos_ms: i64) -> Option<usize> {
        if self.words.is_empty() || pos_ms < self.words[0].0 {
            return None;
        }
        match self.words.binary_search_by_key(&pos_ms, |(at, _)| *at) {
            Ok(i) => Some(i),
            Err(i) => Some(i - 1),
        }
    }

    /// Advances to `pos_ms` and returns the half-open range of words just
    /// released. `None` means nothing was due (paused, seek forward, or the
    /// cursor is already past `pos`).
    pub fn advance_to(&mut self, pos_ms: i64) -> Option<(usize, usize)> {
        let idx = self.index_at(pos_ms)?;
        // Seeked backwards: rewind so the words play again from here.
        if idx + 1 < self.cursor {
            self.cursor = idx;
        }
        if self.cursor > idx {
            return None;
        }
        let start = self.cursor;
        let end = idx + 1;
        self.cursor = end;
        Some((start, end))
    }

    /// Time of the next unreleased word (for wake-up scheduling).
    pub fn next_at(&self) -> Option<i64> {
        self.words.get(self.cursor).map(|(at, _)| *at)
    }

    /// How long the word at `idx` stays on screen.
    ///
    /// Linux parity: 250-500 ms randomised, shortened to fit the gap to the
    /// next word, then capped by `widget_word_ms`, never below 120 ms.
    pub fn display_duration(&self, idx: usize, pos_ms: i64, word_ms: u64, rnd: u64) -> u128 {
        let next_at = self.words.get(idx + 1).map(|x| x.0).unwrap_or(pos_ms + 500);
        release_duration(pos_ms, next_at, word_ms, rnd)
    }
}

pub fn release_duration(pos_ms: i64, next_at: i64, word_ms: u64, rnd: u64) -> u128 {
    let mut dur = 250 + (rnd % 251) as i64;
    dur = dur.min((next_at - pos_ms).max(120));
    dur = dur.min((word_ms as i64).max(120));
    dur.max(120) as u128
}

/// Interpolated player position.
#[derive(Clone, Debug, Default)]
pub struct PlaybackClock {
    pub base_us: i64,
    pub base_at_ms: u128,
    pub playing: u8,
}

impl PlaybackClock {
    pub fn set(&mut self, pos_us: i64, now_ms: u128, playing: u8) {
        self.base_us = pos_us.max(0);
        self.base_at_ms = now_ms;
        self.playing = playing;
    }

    /// Position in ms, extrapolated from the last poll while playing.
    pub fn position_ms(&self, now_ms: u128) -> i64 {
        let mut us = self.base_us;
        if self.playing == 1 {
            us += (now_ms.saturating_sub(self.base_at_ms)) as i64 * 1000;
        }
        (us / 1000).max(0)
    }

    /// Position after subtracting output latency (what the listener hears).
    pub fn sync_position_ms(&self, now_ms: u128, latency_ms: i64) -> i64 {
        (self.position_ms(now_ms) - latency_ms).max(0)
    }
}

/// Resolves `widget_latency_ms`: `< 0` means "automatic".
///
/// The Linux build asks PulseAudio whether the sink is Bluetooth; the native
/// backends report `Bluetooth` only when they can detect the default output,
/// otherwise 0 like an analog/digital sink.
pub fn resolve_latency(configured: i64, bluetooth: bool) -> i64 {
    if configured >= 0 {
        configured
    } else if bluetooth {
        200
    } else {
        0
    }
}

/// Anti-desync guard: when the player's position consistently disagrees with
/// where the cursor is, shift the schedule by the rounded average.
#[derive(Clone, Debug, Default)]
pub struct DriftGuard {
    pub ema: f64,
    pub samples: u32,
    pub offset_ms: i64,
}

impl DriftGuard {
    pub fn reset(&mut self) {
        self.ema = 0.0;
        self.samples = 0;
        self.offset_ms = 0;
    }

    /// Returns true when an offset correction was applied.
    pub fn observe(
        &mut self,
        expected_ms: i64,
        pos_ms: i64,
        enabled: bool,
        max_drift_ms: i64,
    ) -> bool {
        if !enabled {
            return false;
        }
        let drift = (pos_ms - expected_ms) as f64;
        self.ema = if self.samples == 0 {
            drift
        } else {
            self.ema * 0.7 + drift * 0.3
        };
        self.samples += 1;
        if self.samples >= 6 && self.ema.abs() > max_drift_ms as f64 {
            let corr = (-self.ema).round() as i64;
            self.offset_ms = (self.offset_ms + corr).clamp(-10_000, 10_000);
            self.ema = 0.0;
            self.samples = 0;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words() -> Vec<(i64, String)> {
        vec![
            (1_000, "one".into()),
            (1_500, "two".into()),
            (2_000, "three".into()),
            (4_000, "four".into()),
        ]
    }

    #[test]
    fn cursor_releases_each_word_once() {
        let mut t = Timeline::new(words());
        assert_eq!(t.advance_to(900), None);
        assert_eq!(t.advance_to(1_000), Some((0, 1)));
        assert_eq!(t.advance_to(1_600), Some((1, 2)));
        // still inside the same word: nothing new
        assert_eq!(t.advance_to(1_900), None);
        assert_eq!(t.advance_to(2_100), Some((2, 3)));
        assert_eq!(t.advance_to(9_000), Some((3, 4)));
        assert_eq!(t.advance_to(9_000), None);
        assert_eq!(t.cursor, 4);
    }

    #[test]
    fn seek_back_rewinds_without_drift() {
        let mut t = Timeline::new(words());
        t.advance_to(4_500);
        assert_eq!(t.cursor, 4);
        // jump back to 2.1s: the word at that index is released again
        assert_eq!(t.advance_to(2_100), Some((2, 3)));
        assert_eq!(t.cursor, 3);
        assert_eq!(t.advance_to(1_100), Some((0, 1)));
        assert_eq!(t.advance_to(1_600), Some((1, 2)));
    }

    #[test]
    fn catch_up_bursts_every_missed_word() {
        let mut t = Timeline::new(words());
        let (a, b) = t.advance_to(3_000).unwrap();
        assert_eq!((a, b), (0, 3));
    }

    #[test]
    fn next_at_tracks_the_cursor() {
        let mut t = Timeline::new(words());
        assert_eq!(t.next_at(), Some(1_000));
        t.advance_to(1_200);
        assert_eq!(t.next_at(), Some(1_500));
        t.advance_to(5_000);
        assert_eq!(t.next_at(), None);
    }

    #[test]
    fn durations_are_bounded() {
        // random base 250..500
        assert_eq!(release_duration(1_000, 9_000, 500, 0), 250);
        assert_eq!(release_duration(1_000, 9_000, 500, 250), 500);
        // next word is very close -> shortened but never below 120
        assert_eq!(release_duration(1_000, 1_030, 500, 249), 120);
        // widget_word_ms caps it
        assert_eq!(release_duration(1_000, 9_000, 180, 250), 180);
        // zero config still yields 120
        assert_eq!(release_duration(0, 9_000, 0, 0), 120);
    }

    #[test]
    fn clock_interpolates_only_while_playing() {
        let mut c = PlaybackClock::default();
        c.set(30_000_000, 1_000, 1);
        assert_eq!(c.position_ms(1_500), 30_500);
        assert_eq!(c.sync_position_ms(1_500, 200), 30_300);
        c.set(30_500_000, 1_500, 2);
        assert_eq!(c.position_ms(5_000), 30_500);
    }

    #[test]
    fn latency_resolution() {
        assert_eq!(resolve_latency(180, true), 180);
        assert_eq!(resolve_latency(-1, true), 200);
        assert_eq!(resolve_latency(-1, false), 0);
    }

    #[test]
    fn drift_guard_needs_six_samples_and_exceeds_threshold() {
        let mut g = DriftGuard::default();
        for _ in 0..5 {
            assert!(!g.observe(10_000, 10_400, true, 120));
        }
        assert!(g.observe(10_000, 10_400, true, 120));
        assert_eq!(g.offset_ms, -400);
        assert_eq!(g.samples, 0);
        // tiny drift never corrects
        let mut g = DriftGuard::default();
        for _ in 0..20 {
            assert!(!g.observe(10_000, 10_020, true, 120));
        }
        assert_eq!(g.offset_ms, 0);
        // disabled guard is inert
        let mut g = DriftGuard::default();
        for _ in 0..20 {
            assert!(!g.observe(0, 5_000, false, 120));
        }
        assert_eq!(g.offset_ms, 0);
    }
}
