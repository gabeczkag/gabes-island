//! Headless render benchmark used for the "measured usage" table.
//!
//! It drives the real state machine and the real renderer with a mock player
//! and a mock spectrum, but presents into a null overlay: no window is ever
//! created, so it can run on a CI runner and on a developer machine without
//! anything appearing on screen.
//!
//! ```text
//! gabes-island-native --bench 3
//! ```

use std::time::{Duration, Instant};

use island_core::config::Layout;
use island_core::media::Media;
use island_core::media::PlayerAction;
use tiny_skia::Pixmap;

use crate::app::{App, Mods, Overlay, Surface};
use crate::media::mock::MockMedia;
use crate::media::MediaSource;
use crate::spectrum::mock::MockSpectrum;
use crate::spectrum::SpectrumTracker;

pub struct BenchLine {
    pub label: &'static str,
    /// Loop wake-ups in the simulated interval.
    pub ticks: usize,
    /// Surface presents (pill, waterfall, widget, edit button).
    pub paints: usize,
    /// Wake interval the app asked for, averaged, in ms.
    pub wake_ms: f64,
    /// Milliseconds of work per simulated second.
    pub ms_per_second: f64,
}

impl BenchLine {
    /// Share of one core the renderer would use at this cadence.
    pub fn cpu_percent(&self) -> f64 {
        self.ms_per_second / 10.0
    }
}

struct NullOverlay {
    screen: (i32, i32),
    scale: f32,
    presented: usize,
}

impl Overlay for NullOverlay {
    fn place(&mut self, _s: Surface, _x: i32, _y: i32, _w: i32, _h: i32) {}
    fn present(&mut self, _s: Surface, _pm: &Pixmap) {
        self.presented += 1;
    }
    fn show(&mut self, _s: Surface) {}
    fn hide(&mut self, _s: Surface) {}
    fn screen(&self) -> (i32, i32) {
        self.screen
    }
    fn scale(&self) -> f32 {
        self.scale
    }
}

/// Drives `seconds` of simulated playback and returns µs/frame for each scene.
pub fn run(seconds: u64) -> Vec<BenchLine> {
    let mut out = Vec::new();
    out.push(measure(
        "pill only (idle track)",
        seconds,
        |l| {
            l.wfall = 0;
            l.widgets = 0;
        },
        false,
    ));
    out.push(measure(
        "pill + falling lyrics",
        seconds,
        |l| {
            l.wfall = 1;
            l.wsync = 1;
            l.wsource = island_core::config::SOURCE_LYRICS;
        },
        true,
    ));
    out
}

fn measure(label: &'static str, seconds: u64, tweak: fn(&mut Layout), lyrics: bool) -> BenchLine {
    let mut layout = Layout {
        anim: 0.25,
        ..Layout::default()
    };
    tweak(&mut layout);
    let mut app = App::new(layout, 1.0, (1920, 1080), true);
    let mut media = MockMedia::new();
    app.apply_media(0, media.poll().expect("mock media"));
    if lyrics {
        app.inject_lyrics(
            "[00:01.00]i like the way you move\n[00:04.00]through the night\n\
             [00:07.00]東京の夜\n[00:10.00]hold on tight\n[00:13.00]yes",
        );
    }
    let mut spectrum = SpectrumTracker::new(Box::new(MockSpectrum::new()));
    let mut overlay = NullOverlay {
        screen: (1920, 1080),
        scale: 1.0,
        presented: 0,
    };

    // warm the caches and let the pill finish its entrance animation
    let mut now = 0u128;
    for _ in 0..120 {
        app.update(now, &mut media, &mut spectrum, false);
        app.draw(now, &mut overlay);
        now += 16;
    }

    // Drive the loop at the cadence the app itself asks for, so the numbers
    // map directly onto wall-clock CPU at that cadence.
    let before_paints = overlay.presented;
    let started = Instant::now();
    let mut ticks = 0usize;
    let mut simulated_ms = 0u128;
    let target_ms = seconds.max(1) as u128 * 1000;
    while simulated_ms < target_ms {
        app.update(now, &mut media, &mut spectrum, false);
        app.draw(now, &mut overlay);
        let wake = app.next_wake_ms() as u128;
        now += wake;
        simulated_ms += wake;
        ticks += 1;
    }
    let elapsed = started.elapsed();
    let paints = overlay.presented.saturating_sub(before_paints);
    let ms_per_second = elapsed.as_secs_f64() * 1000.0 / (simulated_ms as f64 / 1000.0);
    let wake_ms = simulated_ms as f64 / ticks.max(1) as f64;

    // keep the compiler from optimising the work away
    std::hint::black_box(overlay.presented);
    let _ = PlayerAction::PlayPause;
    let _ = Mods::default();
    let _ = Media::default();
    let _ = Duration::from_millis(1);
    BenchLine {
        label,
        ticks,
        paints,
        wake_ms,
        ms_per_second,
    }
}

/// Resident set size of this process, in MiB (Linux; `None` elsewhere).
pub fn rss_mib() -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                let kb: f64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kb / 1024.0);
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_runs_without_windows() {
        let lines = run(1);
        assert_eq!(lines.len(), 2);
        for l in &lines {
            assert!(l.ticks > 0);
            assert!(l.paints > 0, "{} never painted", l.label);
            assert!(
                l.ms_per_second < 5_000.0,
                "{} used {:.0} ms/s",
                l.label,
                l.ms_per_second
            );
        }
        if let Some(rss) = rss_mib() {
            assert!(rss > 0.5 && rss < 4_096.0);
        }
    }
}
