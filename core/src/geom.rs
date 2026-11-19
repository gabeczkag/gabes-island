//! Pill geometry, hit boxes and the spring animation.
//!
//! Every number here is copied from the Linux implementation so the pill
//! shrinks, expands and hit-tests identically on all platforms.

use crate::config::{Layout, MAX_BARS, PAD, PILL_MAXW, PILL_MINW};

pub type Rect = (f64, f64, f64, f64);

/// Alpha multiplier used before the pill has animated in.
pub fn anim_alpha(anim: f64) -> f64 {
    if anim > 0.0 {
        anim
    } else {
        0.001
    }
}

/// Far side of the fixed element cluster (spectrum, time, buttons).
pub struct Cluster {
    pub art_w: f64,
    pub art_h: f64,
    pub text_x: f64,
    pub time_x: f64,
    pub time_w: f64,
    pub spec_x: f64,
    pub spec_w: f64,
    pub buttons_x: f64,
    pub btn_area: f64,
    pub text_right: f64,
}

/// Right-to-left layout shared by the renderer, the hit tests and the editor.
pub fn cluster(l: &Layout, w: f64) -> Cluster {
    let art_w = l.art_size();
    let btn_area = 4.0 * l.btn + 3.0 * l.gap;
    let buttons_x = w - PAD - l.padx - btn_area;
    let spec_x = buttons_x - 10.0 - l.spec;
    let time_x = spec_x - 8.0 - l.time;
    let text_right = time_x - 8.0;
    Cluster {
        art_w,
        art_h: l.art_h.clamp(18.0, 120.0),
        text_x: PAD + l.padx + art_w + 10.0,
        time_x,
        time_w: l.time,
        spec_x,
        spec_w: l.spec,
        buttons_x,
        btn_area,
        text_right,
    }
}

/// Pill width for a given text width (the wider of title/artist).
pub fn pill_width(l: &Layout, text_w: f64) -> i32 {
    let btn_area = 4.0 * l.btn + 3.0 * l.gap;
    let w = PAD
        + l.padx
        + l.art_size()
        + 10.0
        + text_w
        + l.extra
        + 8.0
        + l.time
        + 8.0
        + l.spec
        + 10.0
        + btn_area
        + l.padx
        + PAD;
    (w as i32).clamp(PILL_MINW, PILL_MAXW)
}

/// The four transport buttons: prev, play/pause, next, edit.
pub fn button_rects(l: &Layout, w: f64, h: f64) -> [Rect; 4] {
    let bw = l.btn;
    let bh = l.btn_h.clamp(10.0, 48.0);
    let total = 4.0 * l.btn + 3.0 * l.gap;
    let x0 = w - PAD - l.padx - total;
    let y = (h - bh) / 2.0;
    [
        (x0, y, bw, bh),
        (x0 + l.btn + l.gap, y, bw, bh),
        (x0 + 2.0 * (l.btn + l.gap), y, bw, bh),
        (x0 + 3.0 * (l.btn + l.gap), y, bw, bh),
    ]
}

/// Editor boxes: art, text, time, spectrum, buttons.
pub fn element_boxes(l: &Layout, w: f64, h: f64) -> [Rect; 5] {
    let c = cluster(l, w);
    let text_h = l.text_h.clamp(16.0, 90.0);
    let time_h = l.time_h.clamp(10.0, 70.0);
    let spec_h = l.spec_h.clamp(10.0, 90.0);
    let btn_h = l.btn_h.clamp(10.0, 48.0);
    [
        (PAD + l.padx, (h - c.art_h) / 2.0, c.art_w, c.art_h),
        (
            c.text_x - 2.0,
            (h - text_h) / 2.0,
            (c.text_right - c.text_x + 2.0).max(10.0),
            text_h,
        ),
        (c.time_x, (h - time_h) / 2.0, c.time_w, time_h),
        (c.spec_x, (h - spec_h) / 2.0, c.spec_w, spec_h),
        (c.buttons_x, (h - btn_h) / 2.0, c.btn_area, btn_h),
    ]
}

/// Smallest region that must be repainted every frame: spectrum, time and
/// progress. Everything else (pill, art, text, buttons) is cached.
pub fn dynamic_rects(l: &Layout, w: f64, h: f64) -> [(i32, i32, i32, i32); 3] {
    let c = cluster(l, w);
    let spec_h = l.spec_h.clamp(10.0, 90.0);
    let spec_y0 = (h - spec_h) / 2.0;
    let time_h = l.time_h.clamp(10.0, 70.0);
    let time_y0 = (h - time_h) / 2.0;
    let clamp_rect = |x: f64, y: f64, ww: f64, hh: f64| -> (i32, i32, i32, i32) {
        let x0 = x.floor().max(0.0) as i32;
        let y0 = y.floor().max(0.0) as i32;
        let x1 = (x + ww).ceil().min(w) as i32;
        let y1 = (y + hh).ceil().min(h) as i32;
        (x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
    };
    [
        clamp_rect(c.spec_x - 1.0, spec_y0 - 2.0, c.spec_w + 2.0, spec_h + 4.0),
        clamp_rect(c.time_x - 2.0, time_y0 - 1.0, c.time_w + 4.0, time_h + 2.0),
        clamp_rect(c.text_x, h - 9.0, c.text_right - c.text_x, 9.0),
    ]
}

/// One bar of the spectrum.
pub struct BarLayout {
    pub nbars: usize,
    pub slot: f64,
    pub bar_w: f64,
    pub bar_gap: f64,
    pub y0: f64,
    pub height: f64,
    pub x0: f64,
}

impl BarLayout {
    /// Bar `i` as `(x, y, w, h)` for a normalised level 0..1.
    pub fn bar(&self, i: usize, level: f64) -> Rect {
        let bh = self.height * (0.16 + 0.84 * level.clamp(0.0, 1.0));
        (
            self.x0 + i as f64 * self.slot,
            self.y0 + (self.height - bh) / 2.0,
            self.bar_w,
            bh,
        )
    }

    /// Which of the 32 captured bands this bar shows.
    pub fn band(&self, i: usize) -> usize {
        i * MAX_BARS / self.nbars.max(1)
    }
}

pub fn bar_layout(l: &Layout, w: f64, h: f64) -> BarLayout {
    let c = cluster(l, w);
    let spec_area = (l.spec * l.spec_h).max(1.0);
    let nbars = ((spec_area / 140.0).round() as usize).clamp(4, MAX_BARS);
    let slot = l.spec / nbars as f64;
    let bar_w = (slot * 0.62).max(1.2);
    let spec_h = l.spec_h.clamp(10.0, 90.0);
    BarLayout {
        nbars,
        slot,
        bar_w,
        bar_gap: (slot - bar_w).max(0.4),
        y0: (h - spec_h) / 2.0,
        height: spec_h,
        x0: c.spec_x,
    }
}

/// The hide/show animation: a critically-ish damped spring with a small
/// overshoot, exactly as on Linux (`omega = 5 / anim_secs`).
#[derive(Clone, Debug)]
pub struct Spring {
    pub value: f64,
    pub vel: f64,
    pub expanding: bool,
    pub animating: bool,
}

impl Default for Spring {
    fn default() -> Self {
        Spring {
            value: 0.0,
            vel: 0.0,
            expanding: false,
            animating: false,
        }
    }
}

impl Spring {
    pub fn show(&mut self) {
        self.expanding = true;
        self.animating = true;
        self.vel = 0.0;
    }

    pub fn hide(&mut self) {
        self.expanding = false;
        self.animating = true;
    }

    /// Steps the spring. Returns `true` once nothing changed this tick.
    pub fn step(&mut self, dt: f64, anim_secs: f64) -> bool {
        let target = if self.expanding { 1.0 } else { 0.0 };
        let omega = 5.0 / anim_secs.max(0.2);
        let stiffness = omega * omega;
        let damping = 1.8 * omega;
        let accel = stiffness * (target - self.value) - damping * self.vel;
        self.vel += accel * dt;
        self.value += self.vel * dt;
        if self.value < 0.0 {
            self.value = 0.0;
            if self.vel < 0.0 {
                self.vel = 0.0;
            }
        }
        if self.value > 1.08 {
            self.value = 1.08;
        }
        let settled = (target - self.value).abs() < 0.004 && self.vel.abs() < 0.03;
        if settled {
            self.value = target;
            self.vel = 0.0;
            self.animating = false;
            return true;
        }
        false
    }

    /// Current pill size.
    pub fn pill_size(&self, target_w: i32, pill_h: i32) -> (i32, i32) {
        let e = self.value.clamp(0.0, 1.08);
        let w = 48.0 + (target_w as f64 - 48.0) * e;
        let h = 7.0 + (pill_h as f64 - 7.0) * e;
        (w as i32, h as i32)
    }

    /// True when the pill has fully retracted and the window can be unmapped.
    pub fn fully_hidden(&self) -> bool {
        !self.expanding && self.value <= 0.0001 && !self.animating
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout::default()
    }

    #[test]
    fn width_formula_matches_linux() {
        let l = layout();
        // pad 8 + 32 art + 10 + text + 60 extra + 8 + 30 time + 8 + 26 spec
        // + 10 + (4*18 + 3*6 = 90) + 8
        assert_eq!(pill_width(&l, 100.0), 390);
        assert_eq!(pill_width(&l, 0.0), 290);
        assert_eq!(pill_width(&l, 10_000.0), PILL_MAXW);
        assert_eq!(pill_width(&l, -1_000.0), PILL_MINW);
    }

    #[test]
    fn buttons_sit_inside_the_right_padding() {
        let l = layout();
        let rects = button_rects(&l, 400.0, 43.0);
        assert!((rects[3].0 + rects[3].2 - (400.0 - PAD)).abs() < 1e-9);
        assert!((rects[3].0 - rects[0].0 - 3.0 * (l.btn + l.gap)).abs() < 1e-9);
        assert_eq!(rects[0].3, 18.0);
    }

    #[test]
    fn element_boxes_are_inside_the_pill() {
        let l = layout();
        let (w, h) = (400.0, 43.0);
        for (i, b) in element_boxes(&l, w, h).iter().enumerate() {
            assert!(b.0 >= 0.0 && b.1 >= 0.0, "box {i} starts outside");
            assert!(b.0 + b.2 <= w + 1e-9, "box {i} overflows right");
            assert!(b.1 + b.3 <= h + 1e-9, "box {i} overflows bottom");
        }
    }

    #[test]
    fn dynamic_rects_stay_inside() {
        let l = layout();
        for (w, h) in [(240.0, 30.0), (400.0, 43.0), (1000.0, 120.0)] {
            for (x, y, rw, rh) in dynamic_rects(&l, w, h) {
                assert!(x >= 0 && y >= 0);
                assert!(x as f64 + rw as f64 <= w);
                assert!(y as f64 + rh as f64 <= h);
            }
        }
    }

    #[test]
    fn bar_budget_and_mapping() {
        let l = layout();
        let bars = bar_layout(&l, 400.0, 43.0);
        // 26*28/140 = 5.2 -> 5 bars
        assert_eq!(bars.nbars, 5);
        assert_eq!(bars.band(0), 0);
        assert_eq!(bars.band(4), 25);
        let small = Layout {
            spec: 10.0,
            spec_h: 10.0,
            ..Layout::default()
        };
        assert_eq!(bar_layout(&small, 300.0, 30.0).nbars, 4);
        let tall = Layout {
            spec: 140.0,
            spec_h: 90.0,
            ..Layout::default()
        };
        assert_eq!(bar_layout(&tall, 600.0, 120.0).nbars, MAX_BARS);
    }

    #[test]
    fn spring_expands_and_settles() {
        let mut s = Spring::default();
        s.show();
        let mut frames = 0;
        while !s.step(0.016, 1.5) && frames < 1_000 {
            frames += 1;
        }
        assert!(frames > 5 && frames < 200, "took {frames} frames");
        assert_eq!(s.value, 1.0);
        assert_eq!(s.pill_size(400, 43), (400, 43));
        s.hide();
        let mut frames = 0;
        while !s.step(0.016, 1.5) && frames < 1_000 {
            frames += 1;
        }
        assert!(s.fully_hidden());
        assert_eq!(s.pill_size(400, 43), (48, 7));
    }

    #[test]
    fn animation_never_exceeds_the_cap() {
        // damping is 1.8*omega regardless of anim_secs (ratio 0.9), so the
        // motion is a tiny bit underdamped but never visibly bounces; what
        // matters is that it cannot grow past 1.08 and always settles.
        for secs in [0.2, 0.7, 1.5, 3.0, 5.0] {
            let mut s = Spring::default();
            s.show();
            let mut max = 0.0f64;
            let mut settled_at = None;
            for i in 0..4000 {
                if s.step(0.016, secs) {
                    settled_at = Some(i);
                    break;
                }
                max = max.max(s.value);
            }
            assert!(max <= 1.08 + 1e-9, "secs={secs} max={max}");
            assert!(settled_at.is_some(), "secs={secs} never settled");
            assert_eq!(s.value, 1.0);
        }
    }
}
