//! Frequency analysis shared by every audio backend.
//!
//! The Linux app streams a 1024-sample mono window from the sink monitor and
//! runs a Goertzel per band, normalised by a slow peak follower.  Backends
//! (PulseAudio, WASAPI loopback, CoreAudio tap) only have to hand over PCM;
//! this module produces the exact same 32 normalised levels the pill draws.

pub const MAX_BARS: usize = 32;

/// Log-spaced band centres, 60 Hz .. 12 kHz.
pub fn band_hz(i: usize) -> f32 {
    let t = i as f32 / (MAX_BARS - 1) as f32;
    60.0 * (12000.0f32 / 60.0).powf(t)
}

/// Goertzel magnitude of `freq` in `x`, normalised by window length.
pub fn goertzel(x: &[f32], rate: f32, freq: f32) -> f32 {
    let n = x.len();
    if n == 0 || rate <= 0.0 {
        return 0.0;
    }
    let k = (n as f32 * freq / rate).round() as usize;
    let w = 2.0 * std::f32::consts::PI * k as f32 / n as f32;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for &v in x {
        let s0 = v + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    ((s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0)).sqrt() / (n as f32 * 0.5)
}

/// Peak-normalising analyser with the Linux smoothing constants.
pub struct Spectrum {
    pub levels: [f32; MAX_BARS],
    peak: [f32; MAX_BARS],
    rate: f32,
}

impl Spectrum {
    pub fn new(sample_rate: f32) -> Spectrum {
        Spectrum {
            levels: [0.0; MAX_BARS],
            peak: [0.01; MAX_BARS],
            rate: sample_rate.max(8_000.0),
        }
    }

    pub fn bars(&self) -> &[f32; MAX_BARS] {
        &self.levels
    }

    /// Analyses one mono window and smooths the result into `levels`.
    pub fn feed(&mut self, pcm: &[f32]) {
        let n = pcm.len();
        if n < 64 {
            return;
        }
        // Hann window so the window edges do not smear the spectrum.
        let mut windowed: Vec<f32> = pcm
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (n - 1) as f32).cos();
                s * w
            })
            .collect();
        for b in 0..MAX_BARS {
            let p = goertzel(&windowed, self.rate, band_hz(b));
            self.peak[b] = (self.peak[b] * 0.995).max(p).max(0.0005);
            let v = (p / self.peak[b]).powf(0.65).clamp(0.0, 1.0);
            self.levels[b] = self.levels[b] * 0.45 + v * 0.55;
        }
        windowed.clear();
    }

    /// When paused the bars fall back to the idle baseline.
    pub fn decay(&mut self) {
        for v in self.levels.iter_mut() {
            *v *= 0.6;
            if *v < 0.001 {
                *v = 0.0;
            }
        }
    }

    pub fn clear(&mut self) {
        self.levels = [0.0; MAX_BARS];
        self.peak = [0.01; MAX_BARS];
    }
}

/// Deterministic band levels for `--mock` mode and tests: a slow two-tone
/// sweep that looks like music without touching any audio device.
pub fn mock_levels(t_ms: f64, out: &mut [f32; MAX_BARS]) {
    let t = t_ms / 1000.0;
    for (i, v) in out.iter_mut().enumerate() {
        let pos = i as f64 / (MAX_BARS - 1) as f64;
        let centre = 0.35 + 0.22 * (t * 0.7).sin();
        let spread = (1.0 - ((pos - centre).abs() * 5.0)).max(0.0);
        let wobble = 0.5 + 0.5 * (t * (2.0 + i as f64 * 0.4) + i as f64).sin();
        let bass = (1.0 - pos * 2.4).max(0.0) * 0.55;
        *v = ((spread * 0.9 + bass) * wobble).clamp(0.0, 1.0) as f32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / rate).sin() * 0.8)
            .collect()
    }

    #[test]
    fn bands_are_monotonic_log_spaced() {
        assert!((band_hz(0) - 60.0).abs() < 0.01);
        assert!((band_hz(MAX_BARS - 1) - 12_000.0).abs() < 1.0);
        for i in 1..MAX_BARS {
            assert!(band_hz(i) > band_hz(i - 1));
        }
    }

    #[test]
    fn goertzel_peaks_at_its_frequency() {
        let rate = 44_100.0;
        let x = sine(1_000.0, rate, 1024);
        let on = goertzel(&x, rate, 1_000.0);
        let off = goertzel(&x, rate, 4_000.0);
        assert!(on > off * 5.0, "on={on} off={off}");
        assert_eq!(goertzel(&[], rate, 1_000.0), 0.0);
    }

    #[test]
    fn analyser_lights_the_right_band() {
        let rate = 44_100.0;
        let mut s = Spectrum::new(rate);
        let tone = sine(1_000.0, rate, 2048);
        for _ in 0..30 {
            s.feed(&tone);
        }
        let target = (0..MAX_BARS)
            .min_by(|a, b| {
                (band_hz(*a) - 1_000.0)
                    .abs()
                    .partial_cmp(&(band_hz(*b) - 1_000.0).abs())
                    .unwrap()
            })
            .unwrap();
        let loudest = (0..MAX_BARS)
            .max_by(|a, b| s.levels[*a].partial_cmp(&s.levels[*b]).unwrap())
            .unwrap();
        // adjacent bands can share leakage, but the tone must dominate locally
        assert!(
            (loudest as i32 - target as i32).abs() <= 2,
            "loud at {loudest}, target {target}"
        );
        assert!(s.levels[target] > 0.5);
        assert!(s.levels.iter().all(|v| (0.0..=1.0).contains(v)));
    }

    #[test]
    fn silence_and_decay_fall_to_zero() {
        let mut s = Spectrum::new(44_100.0);
        s.feed(&sine(440.0, 44_100.0, 2048));
        assert!(s.levels.iter().any(|v| *v > 0.1));
        for _ in 0..200 {
            s.decay();
        }
        assert!(s.levels.iter().all(|v| *v == 0.0));
        s.clear();
        assert_eq!(s.bars(), &[0.0; MAX_BARS]);
    }

    #[test]
    fn mock_levels_are_bounded() {
        let mut out = [0.0f32; MAX_BARS];
        for step in 0..40 {
            mock_levels(step as f64 * 97.0, &mut out);
            assert!(out.iter().all(|v| (0.0..=1.0).contains(v)));
        }
    }
}
