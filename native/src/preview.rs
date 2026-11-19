//! Offscreen preview: renders the real surfaces to PNG files.
//!
//! This runs headless, so it is both my visual check while porting and a CI
//! smoke test that the whole render pipeline works on every runner:
//!
//! ```text
//! gabes-island-native --preview ./preview
//! ```

use std::path::{Path, PathBuf};

use island_core::config::{Layout, MAX_BARS};
use island_core::fall::{self, FallWord};
use island_core::rng::Rng;
use island_core::spectrum::mock_levels;
use tiny_skia::{Pixmap, PixmapPaint, Transform};

use crate::paint::{Canvas, ACCENT};
use crate::render::{self, PillFrame, WidgetFrame};
use crate::text::{Face, TextEngine};

const SCALE: f32 = 2.0;

fn levels(now_ms: f64) -> [f32; MAX_BARS] {
    let mut out = [0.0f32; MAX_BARS];
    mock_levels(now_ms, &mut out);
    out
}

fn save(pm: &Pixmap, path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    pm.save_png(path)
        .map_err(|e| std::io::Error::other(format!("{e:?}")))
}

/// Renders every preview into `dir`; returns the files written.
pub fn write_previews(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut engine = TextEngine::new();
    let layout = Layout::default();
    let mut out = Vec::new();

    // 1. the pill as it looks during playback
    let w = 420.0;
    let h = 52.0;
    let frame = PillFrame {
        layout: &layout,
        width: w,
        height: h,
        anim: 1.0,
        title: "Get Lucky".into(),
        artist: "Daft Punk, Pharrell Williams".into(),
        time: "1:05".into(),
        progress: Some(0.42),
        levels: levels(3_000.0),
        playing: true,
        art: None,
        hover_btn: 1,
        edit_mode: false,
        edit_sel: -1,
    };
    let pm = render::render_pill(&mut engine, &frame, SCALE).expect("pill pixmap");
    let path = dir.join("pill.png");
    save(&pm, &path)?;
    out.push(path);

    // 2. CJK title -> CJK face, no tofu
    let cjk = PillFrame {
        title: "夜に駆ける".into(),
        artist: "YOASOBI • 東京".into(),
        ..PillFrame {
            layout: &layout,
            width: w,
            height: h,
            anim: 1.0,
            title: String::new(),
            artist: String::new(),
            time: "3:12".into(),
            progress: Some(0.78),
            levels: levels(7_500.0),
            playing: true,
            art: None,
            hover_btn: -1,
            edit_mode: false,
            edit_sel: -1,
        }
    };
    let pm = render::render_pill(&mut engine, &cjk, SCALE).expect("cjk pixmap");
    let path = dir.join("pill-cjk.png");
    save(&pm, &path)?;
    out.push(path);

    // 3. edit mode with the text box selected
    let edit = PillFrame {
        layout: &layout,
        width: w,
        height: h,
        anim: 1.0,
        title: "Get Lucky".into(),
        artist: "anim 1.5s • tekst • kółko = szer. • Alt = wys.".into(),
        time: "1:05".into(),
        progress: Some(0.42),
        levels: levels(3_000.0),
        playing: true,
        art: None,
        hover_btn: -1,
        edit_mode: true,
        edit_sel: 1,
    };
    let pm = render::render_pill(&mut engine, &edit, SCALE).expect("edit pixmap");
    let path = dir.join("pill-edit.png");
    save(&pm, &path)?;
    out.push(path);

    // 4. random word widget (3D typography)
    let wf = WidgetFrame {
        text: "kod się kompiluje",
        width: 300.0,
        height: 69.0,
        size: 24.0,
        font: 0,
        color: [1.0, 0.90, 0.40],
        bold: true,
        italic: false,
        depth: 4,
        angle: 0.08,
        phase: 0.5,
        three_d: true,
    };
    let pm = render::render_widget(&mut engine, &wf, SCALE).expect("widget pixmap");
    let path = dir.join("widget.png");
    save(&pm, &path)?;
    out.push(path);

    // 5. the waterfall column with trails
    let mut rng = Rng::new(0xC0FFEE);
    let mut words: Vec<FallWord> = Vec::new();
    for (i, text) in [
        "i", "like", "the", "way", "you", "move", "through", "the", "night", "東京", "night", "yes",
    ]
    .iter()
    .enumerate()
    {
        let mut w = fall::spawn(
            (*text).to_string(),
            120.0 + (i as f64 * 47.0) % 320.0,
            40.0 + (i as f64 * 61.0) % 620.0,
            0,
            &mut rng,
        );
        w.angle = (i as f64 - 6.0) * 0.11;
        w.born = 0;
        words.push(w);
    }
    let mut pm = Pixmap::new((560.0 * SCALE) as u32, (760.0 * SCALE) as u32).unwrap();
    render::draw_waterfall(&mut pm, &mut engine, &words, true, 900, SCALE);
    let path = dir.join("waterfall.png");
    save(&pm, &path)?;
    out.push(path);

    // 6. the edit-mode sliders button
    let pm = render::render_edit_button(&mut engine, true, false, SCALE).expect("edit button");
    let path = dir.join("edit-button.png");
    save(&pm, &path)?;
    out.push(path);

    // 7. everything composited on a desktop-sized background
    let path = dir.join("desktop.png");
    save(&composite(&mut engine, &layout, &words), &path)?;
    out.push(path);

    Ok(out)
}

/// A 1920x1080 mock desktop: pill at the top, waterfall and a widget below.
pub fn composite(engine: &mut TextEngine, layout: &Layout, words: &[FallWord]) -> Pixmap {
    let (sw, sh) = (1920u32, 1080u32);
    let mut screen = Pixmap::new(sw, sh).unwrap();
    {
        let mut c = Canvas::new(&mut screen, 1.0, engine);
        // wallpaper-ish gradient so the transparent surfaces read correctly
        c.fill_rounded_gradient(
            0.0,
            0.0,
            sw as f64,
            sh as f64,
            0.0,
            [0.10, 0.13, 0.22, 1.0],
            [0.03, 0.05, 0.09, 1.0],
        );
        c.fill_circle(1600.0, 180.0, 320.0, [0.20, 0.30, 0.55], 0.25);
        c.fill_circle(260.0, 880.0, 260.0, [0.35, 0.20, 0.45], 0.22);
    }

    let pill_w = 420.0;
    let pill_h = 52.0;
    let frame = PillFrame {
        layout,
        width: pill_w,
        height: pill_h,
        anim: 1.0,
        title: "Get Lucky".into(),
        artist: "Daft Punk, Pharrell Williams".into(),
        time: "1:05".into(),
        progress: Some(0.42),
        levels: levels(3_000.0),
        playing: true,
        art: None,
        hover_btn: -1,
        edit_mode: false,
        edit_sel: -1,
    };
    let pill = render::render_pill(engine, &frame, 1.0).expect("pill");
    let px = (sw as f64 / 2.0 - pill_w / 2.0) as i32;
    screen.draw_pixmap(
        px,
        layout.top,
        pill.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );

    // waterfall column under the pill
    let rect = fall::waterfall_rect(
        sw as i32,
        sh as i32,
        px,
        pill_w as i32,
        layout.top,
        pill_h as i32,
    );
    let mut fall_pm = Pixmap::new(rect.w as u32, rect.h as u32).unwrap();
    render::draw_waterfall(&mut fall_pm, engine, words, true, 900, 1.0);
    screen.draw_pixmap(
        rect.x,
        rect.y,
        fall_pm.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );

    // a widget word floating near the island
    let wf = WidgetFrame {
        text: "prawie działa",
        width: 260.0,
        height: 64.0,
        size: 22.0,
        font: 0,
        color: ACCENT,
        bold: true,
        italic: false,
        depth: 3,
        angle: -0.05,
        phase: 0.5,
        three_d: true,
    };
    if let Some(w) = render::render_widget(engine, &wf, 1.0) {
        screen.draw_pixmap(
            1230,
            90,
            w.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    }
    screen
}

/// Single-file convenience used by `--screenshot`.
pub fn screenshot(path: &Path) -> std::io::Result<PathBuf> {
    let mut engine = TextEngine::new();
    let layout = Layout::default();
    let mut rng = Rng::new(7);
    let words: Vec<FallWord> = (0..8)
        .map(|i| {
            let mut w = fall::spawn(
                format!("word{}", i + 1),
                120.0 + i as f64 * 40.0,
                60.0 + i as f64 * 70.0,
                0,
                &mut rng,
            );
            w.angle = (i as f64 - 4.0) * 0.13;
            w
        })
        .collect();
    let pm = composite(&mut engine, &layout, &words);
    save(&pm, path)?;
    Ok(path.to_path_buf())
}

/// Text helper used by `--diagnose`: reports which families were resolved.
pub fn font_report(engine: &mut TextEngine) -> String {
    let f = &engine.fonts;
    format!(
        "fonts: latin={} alt={} cjk_jp={} cjk_kr={}",
        f.latin, f.latin_alt, f.cjk_jp, f.cjk_kr
    )
}

/// Renders a line of text at 24 px for the font diagnostic.
pub fn font_sample(engine: &mut TextEngine, text: &str) -> bool {
    let mut pm = Pixmap::new(400, 80).unwrap();
    {
        let mut c = Canvas::new(&mut pm, 2.0, engine);
        c.text(
            text,
            4.0,
            30.0,
            24.0,
            false,
            false,
            Face::for_text(text),
            [1.0, 1.0, 1.0],
            1.0,
        );
    }
    pm.pixels().iter().any(|p| p.alpha() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_render_and_save() {
        let dir = std::env::temp_dir().join(format!("island-preview-{}", std::process::id()));
        let files = write_previews(&dir).expect("previews");
        assert_eq!(files.len(), 7);
        for f in &files {
            let meta = std::fs::metadata(f).expect("file exists");
            assert!(meta.len() > 500, "{} is tiny", f.display());
        }
        let png = std::fs::read(&files[0]).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
