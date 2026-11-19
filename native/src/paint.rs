//! A thin drawing surface over tiny-skia.
//!
//! Drawing helpers take their geometry as explicit arguments (x, y, w, h, …)
//! rather than half a dozen tiny structs, so the argument count is deliberate.
#![allow(clippy::too_many_arguments)]
//!
//! All geometry is given in *logical* pixels (the same units as `island.conf`)
//! and converted to the physical pixmap with `scale`. The platform layer picks
//! `scale = window.scale_factor()`, so a 43 px pill is 43 points tall on a
//! Retina display instead of 43 device pixels.

use tiny_skia::{
    FillRule, FilterQuality, LinearGradient, Mask, Paint, Path, PathBuilder, Pixmap, PixmapPaint,
    Point, Shader, SpreadMode, Stroke, StrokeDash, Transform,
};

use crate::text::{Face, TextEngine};

pub type Rgba = [f64; 4];

pub const ACCENT: [f64; 3] = island_core::fall::DEFAULT_ACCENT;
pub const PILL_BG: Rgba = [0.035, 0.04, 0.06, 0.88];
pub const TEXT_PRIMARY: [f64; 3] = [0.90, 0.92, 0.97];
pub const TEXT_SECONDARY: [f64; 3] = [0.56, 0.60, 0.70];
pub const TEXT_TIME: [f64; 3] = [0.60, 0.64, 0.74];
pub const TEXT_BUTTON: [f64; 3] = [0.88, 0.90, 0.96];

/// Rounded-rectangle outline as a fillable path (physical pixels).
pub fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Option<Path> {
    if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = radius.min(h / 2.0).min(w / 2.0).max(0.0);
    if r <= 0.01 {
        let rect = tiny_skia::Rect::from_xywh(x, y, w, h)?;
        return Some(PathBuilder::from_rect(rect));
    }
    let k = 0.552_284_8 * r;
    let (x0, y0, x1, y1) = (x, y, x + w, y + h);
    let mut b = PathBuilder::new();
    b.move_to(x0 + r, y0);
    b.line_to(x1 - r, y0);
    b.cubic_to(x1 - r + k, y0, x1, y0 + r - k, x1, y0 + r);
    b.line_to(x1, y1 - r);
    b.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
    b.line_to(x0 + r, y1);
    b.cubic_to(x0 + r - k, y1, x0, y1 - r + k, x0, y1 - r);
    b.line_to(x0, y0 + r);
    b.cubic_to(x0, y0 + r - k, x0 + r - k, y0, x0 + r, y0);
    b.close();
    b.finish()
}

pub struct Canvas<'a> {
    pub pixmap: &'a mut Pixmap,
    pub scale: f32,
    pub text: &'a mut TextEngine,
}

impl<'a> Canvas<'a> {
    pub fn new(pixmap: &'a mut Pixmap, scale: f32, text: &'a mut TextEngine) -> Canvas<'a> {
        Canvas {
            pixmap,
            scale: if scale > 0.0 { scale } else { 1.0 },
            text,
        }
    }

    fn px(&self, v: f64) -> f32 {
        (v * self.scale as f64) as f32
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn width(&self) -> f64 {
        (self.pixmap.width() as f64) / self.scale as f64
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn height(&self) -> f64 {
        (self.pixmap.height() as f64) / self.scale as f64
    }

    pub fn clear(&mut self) {
        self.pixmap.fill(tiny_skia::Color::TRANSPARENT);
    }

    fn solid(color: [f64; 3], alpha: f64) -> Paint<'static> {
        let mut paint = Paint::default();
        let a = alpha.clamp(0.0, 1.0);
        let q = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        paint.shader = Shader::SolidColor(tiny_skia::Color::from_rgba8(
            q(color[0]),
            q(color[1]),
            q(color[2]),
            (a * 255.0).round() as u8,
        ));
        paint.anti_alias = true;
        paint
    }

    pub fn fill_rounded(
        &mut self,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        radius: f64,
        color: [f64; 3],
        alpha: f64,
    ) {
        if alpha <= 0.001 {
            return;
        }
        if let Some(path) = rounded_rect_path(
            self.px(x),
            self.px(y),
            self.px(w),
            self.px(h),
            self.px(radius),
        ) {
            self.pixmap.fill_path(
                &path,
                &Self::solid(color, alpha),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    pub fn fill_rounded_gradient(
        &mut self,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        radius: f64,
        top: Rgba,
        bottom: Rgba,
    ) {
        let Some(path) = rounded_rect_path(
            self.px(x),
            self.px(y),
            self.px(w),
            self.px(h),
            self.px(radius),
        ) else {
            return;
        };
        let c = |v: Rgba| {
            let q = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
            tiny_skia::Color::from_rgba8(q(v[0]), q(v[1]), q(v[2]), q(v[3]))
        };
        let stops = [
            tiny_skia::GradientStop::new(0.0, c(top)),
            tiny_skia::GradientStop::new(1.0, c(bottom)),
        ];
        let Some(shader) = LinearGradient::new(
            Point::from_xy(self.px(x), self.px(y)),
            Point::from_xy(self.px(x), self.px(y + h)),
            stops.to_vec(),
            SpreadMode::Pad,
            Transform::identity(),
        ) else {
            return;
        };
        let paint = Paint {
            shader,
            anti_alias: true,
            ..Default::default()
        };
        self.pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    pub fn stroke_rounded(
        &mut self,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        radius: f64,
        width: f64,
        color: [f64; 3],
        alpha: f64,
    ) {
        self.stroke_rounded_dashed(x, y, w, h, radius, width, color, alpha, &[]);
    }

    pub fn stroke_rounded_dashed(
        &mut self,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        radius: f64,
        width: f64,
        color: [f64; 3],
        alpha: f64,
        dash: &[f64],
    ) {
        if alpha <= 0.001 {
            return;
        }
        let Some(path) = rounded_rect_path(
            self.px(x),
            self.px(y),
            self.px(w),
            self.px(h),
            self.px(radius),
        ) else {
            return;
        };
        let dashes: Vec<f32> = dash.iter().map(|d| self.px(*d)).collect();
        let stroke = Stroke {
            width: self.px(width).max(1.0 / self.scale),
            dash: if dashes.is_empty() {
                None
            } else {
                StrokeDash::new(dashes, 0.0)
            },
            ..Stroke::default()
        };
        self.pixmap.stroke_path(
            &path,
            &Self::solid(color, alpha),
            &stroke,
            Transform::identity(),
            None,
        );
    }

    pub fn line(
        &mut self,
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        width: f64,
        color: [f64; 3],
        alpha: f64,
    ) {
        if alpha <= 0.001 {
            return;
        }
        let mut b = PathBuilder::new();
        b.move_to(self.px(x0), self.px(y0));
        b.line_to(self.px(x1), self.px(y1));
        let Some(path) = b.finish() else { return };
        let stroke = Stroke {
            width: self.px(width).max(0.6),
            ..Stroke::default()
        };
        self.pixmap.stroke_path(
            &path,
            &Self::solid(color, alpha),
            &stroke,
            Transform::identity(),
            None,
        );
    }

    pub fn fill_circle(&mut self, cx: f64, cy: f64, r: f64, color: [f64; 3], alpha: f64) {
        if alpha <= 0.001 || r <= 0.0 {
            return;
        }
        let mut b = PathBuilder::new();
        b.push_circle(self.px(cx), self.px(cy), self.px(r));
        if let Some(path) = b.finish() {
            self.pixmap.fill_path(
                &path,
                &Self::solid(color, alpha),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    pub fn fill_polygon(&mut self, points: &[(f64, f64)], color: [f64; 3], alpha: f64) {
        if points.len() < 3 || alpha <= 0.001 {
            return;
        }
        let mut b = PathBuilder::new();
        b.move_to(self.px(points[0].0), self.px(points[0].1));
        for (x, y) in &points[1..] {
            b.line_to(self.px(*x), self.px(*y));
        }
        b.close();
        if let Some(path) = b.finish() {
            self.pixmap.fill_path(
                &path,
                &Self::solid(color, alpha),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn fill_dot_rect(
        &mut self,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        radius: f64,
        color: [f64; 3],
        alpha: f64,
    ) {
        self.fill_rounded(x, y, w, h, radius, color, alpha);
    }

    /// Draws `img` into `dst`, scaled to fit (letterboxed) and clipped to a
    /// rounded rectangle — the album-art treatment.
    pub fn image_contain(
        &mut self,
        img: &Pixmap,
        dst: (f64, f64, f64, f64),
        radius: f64,
        alpha: f64,
    ) {
        let (dx, dy, dw, dh) = dst;
        let pw = self.px(dw).round() as u32;
        let ph = self.px(dh).round() as u32;
        if pw == 0 || ph == 0 {
            return;
        }
        let Some(mut tmp) = Pixmap::new(pw, ph) else {
            return;
        };
        let s = (pw as f32 / img.width() as f32).min(ph as f32 / img.height() as f32);
        let ox = (pw as f32 - img.width() as f32 * s) / 2.0;
        let oy = (ph as f32 - img.height() as f32 * s) / 2.0;
        let tf = Transform::from_translate(ox, oy).pre_concat(Transform::from_scale(s, s));
        tmp.draw_pixmap(
            0,
            0,
            img.as_ref(),
            &PixmapPaint {
                quality: FilterQuality::Bilinear,
                opacity: alpha.clamp(0.0, 1.0) as f32,
                blend_mode: tiny_skia::BlendMode::SourceOver,
            },
            tf,
            None,
        );
        if let Some(mut mask) = Mask::new(pw, ph) {
            if let Some(path) = rounded_rect_path(0.0, 0.0, pw as f32, ph as f32, self.px(radius)) {
                mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
                tmp.apply_mask(&mask);
            }
        }
        self.pixmap.draw_pixmap(
            self.px(dx).round() as i32,
            self.px(dy).round() as i32,
            tmp.as_ref(),
            &PixmapPaint {
                quality: FilterQuality::Bilinear,
                ..Default::default()
            },
            Transform::identity(),
            None,
        );
    }

    /// Width of a run in logical pixels.
    pub fn measure(&mut self, text: &str, size: f64, bold: bool, italic: bool, face: Face) -> f64 {
        (self.text.measure(text, self.px(size), bold, italic, face) as f64) / self.scale as f64
    }

    pub fn ellipsize(
        &mut self,
        text: &str,
        max_w: f64,
        size: f64,
        bold: bool,
        italic: bool,
        face: Face,
    ) -> String {
        self.text
            .ellipsize(text, self.px(max_w), self.px(size), bold, italic, face)
    }

    /// Text with its baseline at (`x`, `y`) in logical pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn text(
        &mut self,
        text: &str,
        x: f64,
        baseline: f64,
        size: f64,
        bold: bool,
        italic: bool,
        face: Face,
        color: [f64; 3],
        alpha: f64,
    ) {
        self.text_rotated(
            text, x, baseline, size, bold, italic, face, color, alpha, 0.0,
        );
    }

    /// Rotated text; `angle` is in radians around the baseline anchor.
    #[allow(clippy::too_many_arguments)]
    pub fn text_rotated(
        &mut self,
        text: &str,
        x: f64,
        baseline: f64,
        size: f64,
        bold: bool,
        italic: bool,
        face: Face,
        color: [f64; 3],
        alpha: f64,
        angle: f64,
    ) {
        self.text.draw(
            self.pixmap,
            text,
            self.px(x),
            self.px(baseline),
            self.px(size),
            bold,
            italic,
            face,
            color,
            alpha,
            if angle == 0.0 {
                None
            } else {
                Some(angle as f32)
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_paths_are_inside_their_box() {
        let p = rounded_rect_path(10.0, 5.0, 100.0, 40.0, 20.0).unwrap();
        let b = p.bounds();
        assert!((b.left() - 10.0).abs() < 0.01);
        assert!((b.top() - 5.0).abs() < 0.01);
        assert!((b.right() - 110.0).abs() < 0.01);
        assert!((b.bottom() - 45.0).abs() < 0.01);
        assert!(rounded_rect_path(0.0, 0.0, 0.0, 10.0, 2.0).is_none());
    }

    #[test]
    fn canvas_scales_geometry() {
        let mut pm = Pixmap::new(200, 100).unwrap();
        let mut t = TextEngine::new();
        let mut c = Canvas::new(&mut pm, 2.0, &mut t);
        assert_eq!((c.width(), c.height()), (100.0, 50.0));
        c.fill_rounded(0.0, 0.0, 50.0, 25.0, 12.5, [1.0, 0.0, 0.0], 1.0);
        // logical 50x25 at scale 2 covers 100x50 device pixels
        assert!(pm.pixel(50, 25).unwrap().red() > 200);
        // the rounded corner is transparent
        assert_eq!(pm.pixel(2, 2).unwrap().alpha(), 0);
        assert_eq!(pm.pixel(150, 80).unwrap().alpha(), 0);
    }

    #[test]
    fn image_contain_letterboxes() {
        let mut art = Pixmap::new(10, 20).unwrap();
        art.fill(tiny_skia::Color::from_rgba8(0, 255, 0, 255));
        let mut pm = Pixmap::new(100, 100).unwrap();
        let mut t = TextEngine::new();
        let mut c = Canvas::new(&mut pm, 1.0, &mut t);
        c.image_contain(&art, (0.0, 0.0, 40.0, 40.0), 0.0, 1.0);
        // scaled to 20x40, centred horizontally: x=10..30
        assert!(pm.pixel(20, 20).unwrap().green() > 200);
        assert_eq!(pm.pixel(2, 20).unwrap().alpha(), 0);
    }
}
