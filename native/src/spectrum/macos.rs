//! macOS spectrum — **not implemented yet**.
//!
//! macOS has no WASAPI-style loopback. The supported route is a Core Audio
//! process tap (`AudioHardwareCreateProcessTap`, macOS 14.4+) or
//! ScreenCaptureKit audio capture; both need a user permission prompt and the
//! tap needs the app to be signed with the audio-capture entitlement. Neither
//! can be built or verified from this repository's CI yet, so the port ships
//! the honest placeholder: the spectrum area draws the same idle 16 % bars a
//! paused track shows, and everything else (title, art, time, progress,
//! buttons, lyrics, waterfall) works normally.
//!
//! Track this in the platform notes; when a tap is added it only has to fill
//! `levels` with the 32 band values from `island_core::spectrum::Spectrum`.

use island_core::config::MAX_BARS;

use super::SpectrumSource;

pub struct CoreAudioTap;

impl CoreAudioTap {
    pub fn new() -> CoreAudioTap {
        CoreAudioTap
    }
}

impl SpectrumSource for CoreAudioTap {
    fn levels(&mut self, _playing: bool, _now_ms: u128) -> Option<[f32; MAX_BARS]> {
        None
    }

    fn bluetooth(&self) -> bool {
        false
    }

    fn backend(&self) -> &'static str {
        "coreaudio-tap (stub)"
    }
}
