//! Demo spectrum for `--mock` / previews, and the "no backend" placeholder.

use island_core::config::MAX_BARS;
use island_core::spectrum::mock_levels;

use super::SpectrumSource;

pub struct MockSpectrum {
    levels: [f32; MAX_BARS],
}

impl Default for MockSpectrum {
    fn default() -> Self {
        Self::new()
    }
}

impl MockSpectrum {
    pub fn new() -> MockSpectrum {
        MockSpectrum {
            levels: [0.0; MAX_BARS],
        }
    }
}

impl SpectrumSource for MockSpectrum {
    fn levels(&mut self, playing: bool, now_ms: u128) -> Option<[f32; MAX_BARS]> {
        if playing {
            mock_levels(now_ms as f64, &mut self.levels);
        } else {
            for l in self.levels.iter_mut() {
                *l *= 0.7;
            }
        }
        Some(self.levels)
    }

    fn bluetooth(&self) -> bool {
        false
    }

    fn backend(&self) -> &'static str {
        "mock"
    }
}

/// Used when the platform has no capture backend: every level stays 0 and the
/// pill draws the idle 16 % bars, exactly like a paused track.
#[cfg_attr(not(test), allow(dead_code))]
pub struct SilentSpectrum;

impl SpectrumSource for SilentSpectrum {
    fn levels(&mut self, _playing: bool, _now_ms: u128) -> Option<[f32; MAX_BARS]> {
        None
    }

    fn bluetooth(&self) -> bool {
        false
    }

    fn backend(&self) -> &'static str {
        "silent"
    }
}
