//! Drawing the three surfaces: the now-playing pill, the random word widget
//! and the falling-words waterfall.
#![allow(clippy::too_many_arguments)]
//!
//! Pixel-for-pixel this mirrors `draw_on`/`draw_widget`/`draw_fall` in the
//! Linux `src/main.rs`: same paddings, same fonts sizes, same icon paths and
//! the same alpha values, so both builds look like the same app.

use island_core::config::{Layout, MAX_BARS, PAD};
use island_core::fall::{alpha as fall_alpha, trail_ghost, FallWord};
use island_core::geom;
use tiny_skia::Pixmap;

use crate::paint::{Canvas, ACCENT, PILL_BG, TEXT_BUTTON, TEXT_PRIMARY, TEXT_SECONDARY, TEXT_TIME};
use crate::text::{Face, TextEngine};

/// Everything `draw_pill` needs; built by the app each frame.
pub struct PillFrame<'a> {
    pub layout: &'a Layout,
    pub width: f64,
    pub height: f64,
    pub anim: f64,
    pub title: String,
    pub artist: String,
    pub time: String,
    /// `None` when the track has no known length (progress line hidden).
    pub progress: Option<f64>,
    pub levels: [f32; MAX_BARS],
    pub playing: bool,
    pub art: Option<&'a Pixmap>,
    pub hover_btn: i32,
    pub edit_mode: bool,
    pub edit_sel: i32,
}

/// Face for a run: CJK wins over the widget's Latin choice, as on Linux.
fn face_for(text: &str, alt: bool) -> Face {
    match Face::for_text(text) {
        Face::Latin if alt => Face::LatinAlt,
        other => other,
    }
}

fn text_hh(l: &Layout) -> f64 {
    l.text_h.clamp(16.0, 90.0)
}

fn type_scale(l: &Layout) -> f64 {
    (l.text_h / 26.0).clamp(0.6, 2.0)
}

pub fn pill_pixmap(width: f64, height: f64, scale: f32) -> Option<Pixmap> {
    let w = ((width * scale as f64).round() as u32).max(1);
    let h = ((height * scale as f64).round() as u32).max(1);
    Pixmap::new(w, h)
}

/// Renders the pill into a fresh transparent pixmap of `width`x`height`.
pub fn render_pill(engine: &mut TextEngine, f: &PillFrame, scale: f32) -> Option<Pixmap> {
    let mut pm = pill_pixmap(f.width, f.height, scale)?;
    draw_pill(&mut pm, engine, f, scale);
    Some(pm)
}

pub fn draw_pill(pm: &mut Pixmap, engine: &mut TextEngine, f: &PillFrame, scale: f32) {
    draw_pill_static(pm, engine, f, scale);
    draw_pill_dynamic(pm, engine, f, scale);
}

/// The cached layer: everything that only changes when the track, the layout
/// or the interaction state changes.
pub fn draw_pill_static(pm: &mut Pixmap, engine: &mut TextEngine, f: &PillFrame, scale: f32) {
    let w = f.width;
    let h = f.height;
    let a = geom::anim_alpha(f.anim);
    let l = f.layout;
    let cl = geom::cluster(l, w);
    let mut c = Canvas::new(pm, scale, engine);
    c.clear();

    // ---- the pill itself
    c.fill_rounded(
        0.0,
        0.0,
        w,
        h,
        h / 2.0,
        [PILL_BG[0], PILL_BG[1], PILL_BG[2]],
        PILL_BG[3] * a,
    );
    c.stroke_rounded(
        0.5,
        0.5,
        (w - 1.0).max(1.0),
        (h - 1.0).max(1.0),
        (h - 1.0) / 2.0,
        1.0,
        ACCENT,
        0.28 * a,
    );

    // ---- album art
    let ax = PAD + l.padx;
    let ay = (h - cl.art_h) / 2.0;
    if let Some(art) = f.art {
        c.image_contain(art, (ax, ay, cl.art_w, cl.art_h), 6.0, a);
    } else {
        c.fill_rounded_gradient(
            ax,
            ay,
            cl.art_w,
            cl.art_h,
            6.0,
            [0.30, 0.34, 0.44, a],
            [0.15, 0.17, 0.24, a],
        );
        draw_note(
            &mut c,
            ax + cl.art_w / 2.0,
            ay + cl.art_h / 2.0,
            cl.art_w.min(cl.art_h) / 32.0,
            [1.0, 1.0, 1.0],
            0.9 * a,
        );
    }
    c.stroke_rounded(
        ax + 0.5,
        ay + 0.5,
        (cl.art_w - 1.0).max(1.0),
        (cl.art_h - 1.0).max(1.0),
        6.0,
        1.0,
        [1.0, 1.0, 1.0],
        0.12 * a,
    );

    // ---- title / artist
    let ts = type_scale(l);
    let th = text_hh(l);
    let text_y0 = (h - th) / 2.0;
    let title_base = text_y0 + th * 0.42;
    let artist_base = text_y0 + th * 0.92;
    let maxw = (cl.text_right - cl.text_x - 4.0).max(10.0);

    let tface = face_for(&f.title, false);
    let title = c.ellipsize(&f.title, maxw, 12.0 * ts, true, false, tface);
    c.text(
        &title,
        cl.text_x,
        title_base,
        12.0 * ts,
        true,
        false,
        tface,
        TEXT_PRIMARY,
        a,
    );

    if !f.artist.is_empty() {
        let aface = face_for(&f.artist, false);
        let artist = c.ellipsize(&f.artist, maxw, 10.0 * ts, false, false, aface);
        c.text(
            &artist,
            cl.text_x,
            artist_base,
            10.0 * ts,
            false,
            false,
            aface,
            TEXT_SECONDARY,
            a,
        );
    }

    // ---- transport buttons
    let btns = geom::button_rects(l, w, h);
    for (i, (bx, by, bw, bh)) in btns.iter().enumerate() {
        draw_button(
            &mut c,
            i as i32,
            *bx,
            *by,
            *bw,
            *bh,
            f.hover_btn == i as i32,
            f.playing,
            a,
        );
    }

    // ---- editor boxes
    if f.edit_mode {
        let boxes = geom::element_boxes(l, w, h);
        for (i, b) in boxes.iter().enumerate() {
            let sel = f.edit_sel == i as i32;
            if sel {
                c.fill_rounded(b.0, b.1, b.2, b.3, 4.0, ACCENT, 0.10);
            }
            c.stroke_rounded_dashed(
                b.0,
                b.1,
                b.2,
                b.3,
                4.0,
                if sel { 1.6 } else { 1.0 },
                ACCENT,
                if sel { 0.95 } else { 0.45 },
                if sel { &[] } else { &[3.0, 3.0] },
            );
        }
        let main_sel = f.edit_sel == 5;
        c.stroke_rounded_dashed(
            0.5,
            0.5,
            (w - 1.0).max(1.0),
            (h - 1.0).max(1.0),
            (h - 1.0) / 2.0,
            if main_sel { 1.8 } else { 1.0 },
            ACCENT,
            if main_sel { 0.95 } else { 0.5 } * a,
            if main_sel { &[] } else { &[4.0, 4.0] },
        );
    }
}

/// The per-frame layer: elapsed time, spectrum and progress.
pub fn draw_pill_dynamic(pm: &mut Pixmap, engine: &mut TextEngine, f: &PillFrame, scale: f32) {
    let w = f.width;
    let h = f.height;
    let a = geom::anim_alpha(f.anim);
    let l = f.layout;
    let cl = geom::cluster(l, w);
    let mut c = Canvas::new(pm, scale, engine);

    let time_h = l.time_h.clamp(10.0, 70.0);
    let tw = c.measure(&f.time, 9.0, false, false, Face::Latin);
    c.text(
        &f.time,
        cl.time_x + cl.time_w - tw,
        (h - time_h) / 2.0 + time_h / 2.0 + 3.0,
        9.0,
        false,
        false,
        Face::Latin,
        TEXT_TIME,
        a,
    );

    let bars = geom::bar_layout(l, w, h);
    for i in 0..bars.nbars {
        let band = bars.band(i);
        let lvl = if f.playing {
            f.levels[band] as f64
        } else {
            island_core::fall::IDLE_LEVEL
        };
        let (bx, by, bw, bh) = bars.bar(i, lvl);
        c.fill_rounded(
            bx,
            by,
            bw,
            bh,
            1.2,
            ACCENT,
            if f.playing { 0.9 } else { 0.45 } * a,
        );
    }

    if let Some(p) = f.progress {
        let px0 = cl.text_x;
        let px1 = cl.text_right;
        let pw = (px1 - px0).max(0.0);
        c.fill_rounded(px0, h - 6.0, pw, 1.6, 0.8, [1.0, 1.0, 1.0], 0.08 * a);
        c.fill_rounded(
            px0,
            h - 6.0,
            pw * p.clamp(0.0, 1.0),
            1.6,
            0.8,
            ACCENT,
            0.9 * a,
        );
    }
}

fn draw_note(c: &mut Canvas, cx: f64, cy: f64, k: f64, color: [f64; 3], alpha: f64) {
    // two note heads, a stem and a beam — the Linux placeholder glyph
    c.fill_circle(cx - 4.0 * k, cy + 3.2 * k, 4.7 * k, color, alpha);
    c.fill_circle(cx + 3.4 * k, cy + 1.2 * k, 4.7 * k, color, alpha);
    c.fill_rounded(
        cx - 0.6 * k,
        cy - 10.5 * k,
        1.6 * k,
        14.5 * k,
        0.8 * k,
        color,
        alpha,
    );
    c.fill_rounded(
        cx + 6.2 * k,
        cy - 12.5 * k,
        1.6 * k,
        14.5 * k,
        0.8 * k,
        color,
        alpha,
    );
    c.fill_polygon(
        &[
            (cx - 0.6 * k, cy - 10.5 * k),
            (cx + 7.8 * k, cy - 12.5 * k),
            (cx + 7.8 * k, cy - 8.6 * k),
            (cx - 0.6 * k, cy - 6.6 * k),
        ],
        color,
        alpha,
    );
}

fn draw_button(
    c: &mut Canvas,
    i: i32,
    bx: f64,
    by: f64,
    bw: f64,
    bh: f64,
    hover: bool,
    playing: bool,
    a: f64,
) {
    let k = (bw.min(bh) / 18.0).clamp(0.5, 2.0);
    if hover {
        c.fill_rounded(bx, by, bw, bh, 6.0 * k, ACCENT, 0.22 * a);
    }
    let cx = bx + bw / 2.0;
    let cy = by + bh / 2.0;
    let col = if hover { ACCENT } else { TEXT_BUTTON };
    let alpha = 0.95 * a;
    match i {
        0 => {
            c.fill_rounded(
                cx - 6.0 * k,
                cy - 5.0 * k,
                2.0 * k,
                10.0 * k,
                0.3 * k,
                col,
                alpha,
            );
            c.fill_polygon(
                &[
                    (cx + 5.0 * k, cy - 5.0 * k),
                    (cx + 5.0 * k, cy + 5.0 * k),
                    (cx - 3.0 * k, cy),
                ],
                col,
                alpha,
            );
        }
        1 => {
            if playing {
                c.fill_rounded(
                    cx - 4.0 * k,
                    cy - 5.0 * k,
                    3.0 * k,
                    10.0 * k,
                    0.4 * k,
                    col,
                    alpha,
                );
                c.fill_rounded(
                    cx + 1.0 * k,
                    cy - 5.0 * k,
                    3.0 * k,
                    10.0 * k,
                    0.4 * k,
                    col,
                    alpha,
                );
            } else {
                c.fill_polygon(
                    &[
                        (cx - 3.0 * k, cy - 6.0 * k),
                        (cx - 3.0 * k, cy + 6.0 * k),
                        (cx + 6.0 * k, cy),
                    ],
                    col,
                    alpha,
                );
            }
        }
        2 => {
            c.fill_polygon(
                &[
                    (cx - 5.0 * k, cy - 5.0 * k),
                    (cx - 5.0 * k, cy + 5.0 * k),
                    (cx + 3.0 * k, cy),
                ],
                col,
                alpha,
            );
            c.fill_rounded(
                cx + 4.0 * k,
                cy - 5.0 * k,
                2.0 * k,
                10.0 * k,
                0.3 * k,
                col,
                alpha,
            );
        }
        _ => {
            for (yy, kx) in [(-5.0f64, 3.0f64), (0.0, -3.0), (5.0, 1.5)] {
                c.line(
                    cx - 5.0 * k,
                    cy + yy * k,
                    cx + 5.0 * k,
                    cy + yy * k,
                    1.2 * k,
                    col,
                    alpha,
                );
                c.fill_circle(cx + kx * k, cy + yy * k, 1.7 * k, ACCENT, 0.95 * a);
            }
        }
    }
}

/// Alpha envelope of one random word widget from its animation phase.
pub fn widget_alpha(phase: f64) -> f64 {
    if phase < 0.2 {
        (phase / 0.2).clamp(0.0, 1.0)
    } else if phase > 0.8 {
        ((1.0 - phase) / 0.2).clamp(0.0, 1.0)
    } else {
        1.0
    }
}

pub struct WidgetFrame<'a> {
    pub text: &'a str,
    pub width: f64,
    pub height: f64,
    pub size: f64,
    /// 0 = primary Latin, 1 = secondary (the Linux widget's two fonts)
    pub font: usize,
    pub color: [f64; 3],
    pub bold: bool,
    pub italic: bool,
    pub depth: i32,
    pub angle: f64,
    pub phase: f64,
    pub three_d: bool,
}

pub fn render_widget(engine: &mut TextEngine, f: &WidgetFrame, scale: f32) -> Option<Pixmap> {
    let w = ((f.width * scale as f64).round() as u32).max(1);
    let h = ((f.height * scale as f64).round() as u32).max(1);
    let mut pm = Pixmap::new(w, h)?;
    let a = widget_alpha(f.phase);
    {
        let mut c = Canvas::new(&mut pm, scale, engine);
        let fs = f.size.clamp(12.0, 26.0);
        let face = face_for(f.text, f.font == 1);
        let label = c.ellipsize(f.text, f.width - 26.0, fs, f.bold, f.italic, face);
        let angle = if f.three_d {
            f.angle + (f.phase - 0.5) * 0.15
        } else {
            0.0
        };
        let zoom = if f.three_d {
            1.0 + 0.035 * (f.phase * std::f64::consts::PI).sin()
        } else {
            1.0
        };
        pen_word(
            &mut c,
            &label,
            f.width / 2.0,
            f.height / 2.0 + fs * 0.35,
            fs * zoom,
            f.bold,
            f.italic,
            face,
            f.color,
            angle,
            a,
            if f.three_d { f.depth.max(1) } else { 0 },
            f.three_d,
        );
    }
    Some(pm)
}

/// Draws one word whose *visual centre* is (`cx`, `cy`).
///
/// `shadow` > 0 draws the extruded 3D copies; without 3D a plain black drop
/// shadow is used, matching the Linux widget.
#[allow(clippy::too_many_arguments)]
fn pen_word(
    c: &mut Canvas,
    label: &str,
    cx: f64,
    cy: f64,
    size: f64,
    bold: bool,
    italic: bool,
    face: Face,
    color: [f64; 3],
    angle: f64,
    alpha: f64,
    shadow: i32,
    three_d: bool,
) {
    let tw = c.measure(label, size, bold, italic, face);
    // baseline anchor: local (-tw/2, 0) rotated around the centre
    let (sin, cos) = angle.sin_cos();
    let ax = cx + (-tw / 2.0) * cos;
    let ay = cy + (-tw / 2.0) * sin;
    if shadow > 0 {
        for d in (1..=shadow).rev() {
            let (dx, dy) = (d as f64 * 0.8, d as f64 * 0.8);
            let (rx, ry) = (dx * cos - dy * sin, dx * sin + dy * cos);
            c.text_rotated(
                label,
                ax + rx,
                ay + ry,
                size,
                bold,
                italic,
                face,
                [color[0] * 0.35, color[1] * 0.35, color[2] * 0.45],
                0.55 * alpha,
                angle,
            );
        }
    } else if !three_d {
        let (rx, ry) = (1.0 * cos - 1.0 * sin, 1.0 * sin + 1.0 * cos);
        c.text_rotated(
            label,
            ax + rx,
            ay + ry,
            size,
            bold,
            italic,
            face,
            [0.0, 0.0, 0.0],
            0.5 * alpha,
            angle,
        );
    }
    c.text_rotated(
        label,
        ax,
        ay,
        size,
        bold,
        italic,
        face,
        color,
        0.96 * alpha,
        angle,
    );
}

/// One falling word, matching `draw_fall_text` from the Linux app.
pub fn draw_fall_word(c: &mut Canvas, word: &FallWord, x: f64, y: f64, angle: f64, alpha: f64) {
    if alpha <= 0.001 {
        return;
    }
    let face = face_for(&word.text, word.font == 1);
    let tw = c.measure(&word.text, word.size, word.bold, word.italic, face);
    // baseline at local (-tw/2, 0.35*size), rotated around (x, y)
    let (sin, cos) = angle.sin_cos();
    let anchor_x = x + (-tw / 2.0) * cos - (word.size * 0.35) * sin;
    let anchor_y = y + (-tw / 2.0) * sin + (word.size * 0.35) * cos;
    for d in (1..=word.depth).rev() {
        let (dx, dy) = (d as f64 * 0.8, d as f64 * 0.8);
        let (rx, ry) = (dx * cos - dy * sin, dx * sin + dy * cos);
        c.text_rotated(
            &word.text,
            anchor_x + rx,
            anchor_y + ry,
            word.size,
            word.bold,
            word.italic,
            face,
            [
                word.color[0] * 0.35,
                word.color[1] * 0.35,
                word.color[2] * 0.45,
            ],
            0.55 * alpha,
            angle,
        );
    }
    c.text_rotated(
        &word.text,
        anchor_x,
        anchor_y,
        word.size,
        word.bold,
        word.italic,
        face,
        word.color,
        0.96 * alpha,
        angle,
    );
}

/// Renders the waterfall column (transparent background).
pub fn draw_waterfall(
    pm: &mut Pixmap,
    engine: &mut TextEngine,
    words: &[FallWord],
    trails: bool,
    now_ms: u128,
    scale: f32,
) {
    let mut c = Canvas::new(pm, scale, engine);
    c.clear();
    for word in words {
        let alpha = fall_alpha(word, now_ms);
        if alpha <= 0.0 {
            continue;
        }
        if trails {
            // one ghost copy behind the word
            let (gx, gy, ga) = trail_ghost(word, 1.0);
            draw_fall_word(&mut c, word, gx, gy, ga, alpha * 0.32);
        }
        draw_fall_word(&mut c, word, word.x, word.y, word.angle, alpha);
    }
}

/// The 26x26 sliders button that toggles edit mode.
pub fn render_edit_button(
    engine: &mut TextEngine,
    active: bool,
    hover: bool,
    scale: f32,
) -> Option<Pixmap> {
    let size = ((26.0 * scale as f64).round() as u32).max(8);
    let mut pm = Pixmap::new(size, size)?;
    {
        let mut c = Canvas::new(&mut pm, scale, engine);
        c.clear();
        if !active && !hover {
            c.fill_rounded(0.5, 0.5, 25.0, 25.0, 8.0, [0.035, 0.04, 0.06], 0.62);
        }
        if active {
            c.fill_rounded(0.5, 0.5, 25.0, 25.0, 8.0, ACCENT, 0.42);
        } else if hover {
            c.fill_rounded(0.5, 0.5, 25.0, 25.0, 8.0, ACCENT, 0.28);
        }
        c.stroke_rounded(0.5, 0.5, 25.0, 25.0, 8.0, 1.0, ACCENT, 0.55);
        for (y, kx) in [(7.5f64, 16.0f64), (13.0, 10.0), (18.5, 14.0)] {
            c.line(6.0, y, 20.0, y, 1.5, TEXT_BUTTON, 0.9);
            c.fill_circle(kx, y, 2.6, ACCENT, 0.95);
        }
    }
    Some(pm)
}

#[cfg(test)]
mod tests {
    use super::*;
    use island_core::media::Media;

    fn frame(l: &Layout, w: f64, h: f64) -> PillFrame<'_> {
        PillFrame {
            layout: l,
            width: w,
            height: h,
            anim: 1.0,
            title: "Daft Punk".into(),
            artist: "Get Lucky (feat. Pharrell Williams)".into(),
            time: "1:05".into(),
            progress: Some(0.35),
            levels: [0.4; MAX_BARS],
            playing: true,
            art: None,
            hover_btn: 1,
            edit_mode: false,
            edit_sel: -1,
        }
    }

    #[test]
    fn pill_renders_content() {
        let mut engine = TextEngine::new();
        let l = Layout::default();
        let pm = render_pill(&mut engine, &frame(&l, 420.0, 43.0), 1.0).unwrap();
        assert_eq!(pm.width(), 420);
        let opaque = pm.pixels().iter().filter(|p| p.alpha() > 200).count();
        assert!(opaque > 5_000, "pill mostly transparent: {opaque}");
        // corners are outside the rounded pill
        assert_eq!(pm.pixel(1, 1).unwrap().alpha(), 0);
        // centre is the dark pill background
        let centre = pm.pixel(210, 21).unwrap();
        assert!(centre.alpha() > 200);
        let _ = Media::default();
    }

    #[test]
    fn pill_is_transparent_at_the_top_of_the_animation() {
        let mut engine = TextEngine::new();
        let l = Layout::default();
        let mut f = frame(&l, 420.0, 43.0);
        f.anim = 0.0;
        let pm = render_pill(&mut engine, &f, 1.0).unwrap();
        let opaque = pm.pixels().iter().filter(|p| p.alpha() > 40).count();
        assert_eq!(opaque, 0, "should be invisible before animating in");
    }

    #[test]
    fn edit_mode_draws_boxes() {
        let mut engine = TextEngine::new();
        let l = Layout::default();
        let mut f = frame(&l, 420.0, 43.0);
        f.edit_mode = true;
        f.edit_sel = 3;
        let pm = render_pill(&mut engine, &f, 1.0).unwrap();
        assert!(pm.pixels().iter().any(|p| p.alpha() > 0));
    }

    #[test]
    fn widget_fades_with_phase() {
        assert_eq!(widget_alpha(0.0), 0.0);
        assert_eq!(widget_alpha(0.5), 1.0);
        assert_eq!(widget_alpha(1.0), 0.0);
        assert!((widget_alpha(0.1) - 0.5).abs() < 0.01);
        let mut engine = TextEngine::new();
        let f = WidgetFrame {
            text: "kod się kompiluje",
            width: 220.0,
            height: 46.0,
            size: 22.0,
            font: 0,
            color: [1.0, 0.9, 0.4],
            bold: true,
            italic: false,
            depth: 3,
            angle: 0.1,
            phase: 0.5,
            three_d: true,
        };
        let pm = render_widget(&mut engine, &f, 1.0).unwrap();
        assert!(pm.pixels().iter().any(|p| p.alpha() > 0));
    }

    #[test]
    fn waterfall_draws_every_live_word() {
        let mut engine = TextEngine::new();
        let mut rng = island_core::Rng::new(11);
        let mut words = Vec::new();
        for i in 0..6 {
            let mut w = island_core::fall::spawn(
                format!("word{i}"),
                40.0 + i as f64 * 30.0,
                30.0 + i as f64 * 40.0,
                0,
                &mut rng,
            );
            w.angle = 0.3;
            words.push(w);
        }
        let mut pm = Pixmap::new(400, 400).unwrap();
        draw_waterfall(&mut pm, &mut engine, &words, true, 1_000, 1.0);
        assert!(pm.pixels().iter().filter(|p| p.alpha() > 0).count() > 200);
        // after their lifetime they are no longer drawn
        let mut pm2 = Pixmap::new(400, 400).unwrap();
        draw_waterfall(&mut pm2, &mut engine, &words, true, 5_000, 1.0);
        assert_eq!(pm2.pixels().iter().filter(|p| p.alpha() > 0).count(), 0);
    }

    #[test]
    fn scale_factor_doubles_the_surface() {
        let mut engine = TextEngine::new();
        let l = Layout::default();
        let pm = render_pill(&mut engine, &frame(&l, 420.0, 43.0), 2.0).unwrap();
        assert_eq!((pm.width(), pm.height()), (840, 86));
    }
}
