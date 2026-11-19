//! Real-time spectrum backends.
//!
//! Backends hand 32 normalised band levels to the UI. Analysis itself lives in
//! `island_core::spectrum::Spectrum` (Goertzel per log band + peak follower),
//! so PulseAudio, WASAPI loopback and the mock produce identical-looking bars.

use island_core::config::MAX_BARS;

#[cfg(target_os = "macos")]
pub mod macos;
pub mod mock;
#[cfg(windows)]
pub mod windows;

pub trait SpectrumSource {
    /// Latest band levels, or `None` when audio capture is unavailable.
    /// Must be cheap and non-blocking.
    fn levels(&mut self, playing: bool, now_ms: u128) -> Option<[f32; MAX_BARS]>;

    /// Whether the default output is Bluetooth (drives latency compensation).
    fn bluetooth(&self) -> bool;

    fn backend(&self) -> &'static str;
}

pub fn create(mock: bool) -> Box<dyn SpectrumSource> {
    if mock {
        return Box::new(mock::MockSpectrum::new());
    }
    #[cfg(windows)]
    {
        return Box::new(windows::WasapiLoopback::new());
    }
    #[cfg(target_os = "macos")]
    {
        return Box::new(macos::CoreAudioTap::new());
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        eprintln!("gabes-island: no audio capture backend here; spectrum stays idle");
        Box::new(mock::SilentSpectrum)
    }
}

/// Keeps the last non-zero levels and decays them when playback stops.
pub struct SpectrumTracker {
    source: Box<dyn SpectrumSource>,
    levels: [f32; MAX_BARS],
    pub bluetooth: bool,
    last: u128,
}

impl SpectrumTracker {
    pub fn new(source: Box<dyn SpectrumSource>) -> SpectrumTracker {
        let bluetooth = source.bluetooth();
        SpectrumTracker {
            source,
            levels: [0.0; MAX_BARS],
            bluetooth,
            last: 0,
        }
    }

    pub fn backend(&self) -> &'static str {
        self.source.backend()
    }

    pub fn tick(&mut self, playing: bool, now_ms: u128) -> [f32; MAX_BARS] {
        // The analyser updates at ~30 Hz; the UI can ask faster.
        if now_ms.saturating_sub(self.last) >= 30 {
            self.last = now_ms;
            match self.source.levels(playing, now_ms) {
                Some(v) => self.levels = v,
                None => {
                    if !playing {
                        for l in self.levels.iter_mut() {
                            *l *= 0.6;
                            if *l < 0.001 {
                                *l = 0.0;
                            }
                        }
                    }
                }
            }
        }
        self.levels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_holds_levels_when_the_backend_is_silent() {
        let mut t = SpectrumTracker::new(Box::new(mock::SilentSpectrum));
        assert_eq!(t.tick(false, 0), [0.0; MAX_BARS]);
        assert_eq!(t.backend(), "silent");
        let mut m = SpectrumTracker::new(Box::new(mock::MockSpectrum::new()));
        let levels = m.tick(true, 1_000);
        assert!(levels.iter().any(|v| *v > 0.0));
        // idle for a long time: bars fall back to zero
        let mut idle = m.tick(false, 99_000);
        for i in 0..20 {
            idle = m.tick(false, 99_000 + i * 40);
        }
        assert!(idle.iter().all(|v| *v < 0.2));
    }
}
