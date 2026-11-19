//! The falling-words waterfall.
//!
//! Words are spawned around the island's centre with a random sideways push
//! and fall with gravity-ish constant velocity, spinning slowly.  Each word
//! keeps one ghost copy behind it for the trail.  Math only; the renderer
//! decides how a [`FallWord`] looks.

use crate::config::MAX_BARS;
use crate::rng::Rng;

pub const WIDGET_COLORS: [[f64; 3]; 6] = [
    [1.0, 1.0, 1.0],
    [0.48, 0.64, 0.97],
    [1.0, 0.45, 0.75],
    [0.40, 1.0, 0.80],
    [1.0, 0.90, 0.40],
    [1.0, 0.58, 0.30],
];

/// The pill palette accent.
pub const DEFAULT_ACCENT: [f64; 3] = [0.48, 0.64, 0.97];

/// At most this many words exist at once; the oldest is dropped.
pub const MAX_FALL_WORDS: usize = 12;
/// Milliseconds a word lives before it is culled.
pub const FALL_LIFE_MS: u128 = 2200;

#[derive(Clone, Debug)]
pub struct FallWord {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub vx: f64,
    pub vy: f64,
    pub angle: f64,
    pub spin: f64,
    /// 0 = Noto Sans, 1 = DejaVu Sans (see `WIDGET_FONTS`).
    pub font: usize,
    pub size: f64,
    pub color: [f64; 3],
    pub bold: bool,
    pub italic: bool,
    /// Extra ghost copies behind the word.
    pub depth: i32,
    pub born: u128,
    pub life: u128,
}

/// Where the waterfall lives, in screen pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaterfallRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Linux parity: a 560 px column under the pill, clipped to the screen and
/// limited to 760 px tall.
pub fn waterfall_rect(
    screen_w: i32,
    screen_h: i32,
    island_x: i32,
    pill_w: i32,
    top: i32,
    pill_h: i32,
) -> WaterfallRect {
    let cw = 560.min(screen_w).max(200);
    let island_cx = island_x + pill_w / 2;
    let cx = (island_cx - cw / 2).clamp(0, (screen_w - cw).max(0));
    let cy = (top + pill_h).clamp(0, (screen_h - 120).max(0));
    let ch = (screen_h - cy).clamp(120, 760);
    WaterfallRect {
        x: cx,
        y: cy,
        w: cw,
        h: ch,
    }
}

/// Spawn position for a word, relative to the waterfall window's origin.
///
/// `pill_w` widens the spawn cloud; `fall_x0` is the waterfall's left edge.
pub fn spawn_origin(island_x: i32, pill_w: i32, fall_x0: i32, rng: &mut Rng) -> (f64, f64) {
    let cx = island_x + pill_w / 2 - fall_x0;
    let span = (pill_w.max(80) / 2) as u64;
    let jitter = (rng.next_u64() % span.max(1)) as i32 - span as i32 / 2;
    ((cx + jitter) as f64, 4.0)
}

/// Builds a word with Linux's random typography.
pub fn spawn(text: String, x: f64, y: f64, born: u128, rng: &mut Rng) -> FallWord {
    let r1 = rng.next_u64();
    let r2 = rng.next_u64();
    let r3 = rng.next_u64();
    FallWord {
        text,
        x,
        y,
        vx: -55.0 + (rng.next_u64() % 111) as f64,
        vy: 110.0 + (rng.next_u64() % 140) as f64,
        angle: 0.0,
        spin: ((rng.next_u64() % 1000) as f64 / 1000.0 - 0.5) * 1.4,
        font: if r1.is_multiple_of(4) { 1 } else { 0 },
        size: 14.0 + (r2 % 7) as f64,
        color: WIDGET_COLORS[((r3 >> 6) as usize) % WIDGET_COLORS.len()],
        bold: !r3.is_multiple_of(5),
        italic: (r3 >> 3).is_multiple_of(4),
        depth: 1,
        born,
        life: FALL_LIFE_MS,
    }
}

/// Moves the word by `dt` seconds (clamped by the caller to 0.1 s).
pub fn integrate(word: &mut FallWord, dt: f64) {
    word.x += word.vx * dt;
    word.y += word.vy * dt;
    word.angle += word.spin * dt;
}

/// Fade-in over 180 ms, fade-out over the last 500 ms.
pub fn alpha(word: &FallWord, now_ms: u128) -> f64 {
    let age = now_ms.saturating_sub(word.born);
    if age < 180 {
        age as f64 / 180.0
    } else if age + 500 > word.life {
        ((word.life.saturating_sub(age)) as f64 / 500.0).clamp(0.0, 1.0)
    } else {
        1.0
    }
}

pub fn expired(word: &FallWord, now_ms: u128, screen_h: i32) -> bool {
    now_ms.saturating_sub(word.born) >= word.life || word.y > (screen_h + 80) as f64
}

/// Position of the ghost copy `k` steps behind the word.
pub fn trail_ghost(word: &FallWord, k: f64) -> (f64, f64, f64) {
    (
        word.x - word.vx * 0.06 * k,
        word.y - word.vy * 0.06 * k,
        word.angle - word.spin * 0.06 * k,
    )
}

/// Default 32-band idle shape used before audio arrives.
pub const IDLE_LEVEL: f64 = 0.16;

/// The renderer's spectrum slot count is bounded by [`MAX_BARS`].
pub const BARS: usize = MAX_BARS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waterfall_rect_follows_the_pill_and_screen() {
        let r = waterfall_rect(1920, 1080, 760, 400, 10, 43);
        assert_eq!((r.w, r.h), (560, 760));
        assert_eq!(r.y, 53);
        assert_eq!(r.x, 760 + 200 - 280);
        // narrow screen: column shrinks to screen width
        let r = waterfall_rect(300, 200, 0, 240, 10, 43);
        assert_eq!(r.w, 300);
        assert_eq!(r.y, 53);
        assert_eq!(r.h, 147);
        // tiny height clamps to the 120 px floor
        let r = waterfall_rect(800, 100, 300, 200, 90, 43);
        assert_eq!(r.h, 120);
    }

    #[test]
    fn spawn_stays_near_the_island() {
        let mut rng = Rng::new(42);
        for _ in 0..500 {
            let (x, y) = spawn_origin(600, 400, 480, &mut rng);
            // pill centre at 600+200-480 = 320, jitter +-200
            assert!((x - 320.0).abs() <= 200.0, "x={x}");
            assert_eq!(y, 4.0);
        }
    }

    #[test]
    fn spawned_words_are_plausible() {
        let mut rng = Rng::new(7);
        for _ in 0..200 {
            let w = spawn("test".into(), 10.0, 4.0, 0, &mut rng);
            assert!((14.0..21.0).contains(&w.size));
            assert!((110.0..250.0).contains(&w.vy));
            assert!((-55.0..56.0).contains(&w.vx));
            assert!(w.spin.abs() <= 0.7);
            assert!(w.depth >= 1);
            assert_eq!(w.life, 2200);
        }
    }

    #[test]
    fn alpha_envelope() {
        let mut rng = Rng::new(1);
        let w = spawn("w".into(), 0.0, 0.0, 1_000, &mut rng);
        assert!((alpha(&w, 1_090) - 0.5).abs() < 0.01);
        assert_eq!(alpha(&w, 2_000), 1.0);
        assert!((alpha(&w, 2_950) - 0.5).abs() < 0.01);
        assert_eq!(alpha(&w, 3_200), 0.0);
    }

    #[test]
    fn integration_and_expiry() {
        let mut rng = Rng::new(3);
        let mut w = spawn("w".into(), 100.0, 4.0, 0, &mut rng);
        let (vx, vy) = (w.vx, w.vy);
        integrate(&mut w, 0.5);
        assert!((w.x - (100.0 + vx * 0.5)).abs() < 1e-9);
        assert!((w.y - (4.0 + vy * 0.5)).abs() < 1e-9);
        assert!(!expired(&w, 2_000, 1080));
        assert!(expired(&w, 2_201, 1080));
        // below the screen + margin
        let mut w2 = spawn("w".into(), 0.0, 1200.0, 0, &mut rng);
        w2.born = 0;
        assert!(expired(&w2, 100, 1080));
    }

    #[test]
    fn trail_lags_behind() {
        let mut rng = Rng::new(5);
        let w = spawn("w".into(), 0.0, 0.0, 0, &mut rng);
        let (gx, gy, ga) = trail_ghost(&w, 1.0);
        assert!(gx != w.x && gy != w.y && ga != w.angle);
    }
}
