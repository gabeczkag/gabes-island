//! Text shaping and rasterisation: cosmic-text (HarfBuzz-class shaping with
//! system-font fallback) into tiny-skia pixmaps.
//!
//! Layouts and per-glyph sprites are cached, so a title that does not change
//! costs a hash lookup per frame. Sizes and coordinates are *physical* pixels;
//! callers multiply their logical geometry by the window scale factor.

use std::collections::HashMap;
use std::rc::Rc;

use cosmic_text::{
    Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, Style, SwashCache, SwashContent,
    Weight,
};
use island_core::text::{font_candidates, has_cjk, has_hangul};
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

/// Which family group a run of text should use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    Latin,
    /// Secondary Latin family for the random word widgets.
    LatinAlt,
    CjkJp,
    CjkKr,
}

impl Face {
    /// Picks the CJK face for text that needs it (no tofu boxes).
    pub fn for_text(text: &str) -> Face {
        if has_hangul(text) {
            Face::CjkKr
        } else if has_cjk(text) {
            Face::CjkJp
        } else {
            Face::Latin
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct LayoutKey {
    text: String,
    size_q: u32,
    bold: bool,
    italic: bool,
    face: Face,
}

struct LaidText {
    glyphs: Vec<(CacheKey, i32, i32)>,
    width: f32,
    baseline: f32,
}

/// Resolved font family names, one per [`Face`].
#[derive(Clone, Debug)]
pub struct FontSet {
    pub latin: String,
    pub latin_alt: String,
    pub cjk_jp: String,
    pub cjk_kr: String,
}

impl FontSet {
    fn get(&self, face: Face) -> &str {
        match face {
            Face::Latin => &self.latin,
            Face::LatinAlt => &self.latin_alt,
            Face::CjkJp => &self.cjk_jp,
            Face::CjkKr => &self.cjk_kr,
        }
    }
}

pub struct TextEngine {
    font_system: FontSystem,
    swash: SwashCache,
    layouts: HashMap<LayoutKey, Rc<LaidText>>,
    sprites: HashMap<(CacheKey, u32), Option<Pixmap>>,
    pub fonts: FontSet,
}

impl TextEngine {
    /// Loads system fonts once (a few ms) and resolves the platform families.
    pub fn new() -> TextEngine {
        let font_system = FontSystem::new();
        let mut engine = TextEngine {
            font_system,
            swash: SwashCache::new(),
            layouts: HashMap::new(),
            sprites: HashMap::new(),
            fonts: FontSet {
                latin: String::new(),
                latin_alt: String::new(),
                cjk_jp: String::new(),
                cjk_kr: String::new(),
            },
        };
        let c = font_candidates();
        let latin = engine.pick(c.latin);
        let latin_alt = engine.pick(c.alt);
        let cjk_jp = engine.pick(c.cjk_jp);
        let cjk_kr = engine.pick(c.cjk_kr);
        engine.fonts = FontSet {
            latin,
            latin_alt,
            cjk_jp,
            cjk_kr,
        };
        engine
    }

    fn has_family(&self, name: &str) -> bool {
        let needle = name.to_lowercase();
        self.font_system
            .db()
            .faces()
            .any(|f| f.families.iter().any(|(n, _)| n.to_lowercase() == needle))
    }

    fn pick(&mut self, candidates: &[&str]) -> String {
        for name in candidates {
            if self.has_family(name) {
                return (*name).to_string();
            }
        }
        // Nothing matched: let the shaper choose, but keep a readable name.
        candidates
            .first()
            .copied()
            .unwrap_or("sans-serif")
            .to_string()
    }

    fn layout(
        &mut self,
        text: &str,
        size: f32,
        bold: bool,
        italic: bool,
        face: Face,
    ) -> Rc<LaidText> {
        let size = size.max(1.0);
        let key = LayoutKey {
            text: text.to_string(),
            size_q: (size * 16.0).round() as u32,
            bold,
            italic,
            face,
        };
        if let Some(hit) = self.layouts.get(&key) {
            return hit.clone();
        }
        if self.layouts.len() > 512 {
            self.layouts.clear();
        }

        let family = self.fonts.get(face).to_string();
        let metrics = Metrics::new(size, (size * 1.25).max(size + 2.0));
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(None, None);
        let attrs = Attrs::new()
            .family(Family::Name(family.as_str()))
            .weight(if bold { Weight::BOLD } else { Weight::NORMAL })
            .style(if italic { Style::Italic } else { Style::Normal });
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);

        let mut glyphs = Vec::new();
        let mut width = 0.0f32;
        let mut baseline = size * 0.8;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            if run.line_i == 0 {
                baseline = run.line_y;
            }
            for g in run.glyphs {
                let p = g.physical((0.0, 0.0), 1.0);
                glyphs.push((p.cache_key, p.x, p.y));
            }
        }
        let laid = Rc::new(LaidText {
            glyphs,
            width: width.ceil(),
            baseline,
        });
        self.layouts.insert(key, laid.clone());
        laid
    }

    /// Width of the rendered run in physical pixels.
    pub fn measure(&mut self, text: &str, size: f32, bold: bool, italic: bool, face: Face) -> f32 {
        self.layout(text, size, bold, italic, face).width
    }

    /// Shortens `text` until it fits `max_w`, with a trailing ellipsis.
    ///
    /// Same binary search over char boundaries as the Linux app, so both
    /// builds break a long title at the same character.
    pub fn ellipsize(
        &mut self,
        text: &str,
        max_w: f32,
        size: f32,
        bold: bool,
        italic: bool,
        face: Face,
    ) -> String {
        if text.is_empty() || self.measure(text, size, bold, italic, face) <= max_w {
            return text.to_string();
        }
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let mut lo = 0usize;
        let mut hi = chars.len();
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let cut = if mid < chars.len() {
                chars[mid].0
            } else {
                text.len()
            };
            let candidate = format!("{}\u{2026}", &text[..cut]);
            if self.measure(&candidate, size, bold, italic, face) <= max_w {
                lo = mid;
            } else {
                hi = mid.saturating_sub(1);
            }
        }
        let cut = if lo < chars.len() {
            chars[lo].0
        } else {
            text.len()
        };
        format!("{}\u{2026}", &text[..cut])
    }

    /// Glyph sprite tinted with `rgba` (quantised alpha keeps the cache small).
    fn sprite(&mut self, key: CacheKey, rgba: u32) -> Option<&Pixmap> {
        if !self.sprites.contains_key(&(key, rgba)) {
            if self.sprites.len() > 4096 {
                self.sprites.clear();
            }
            let built = self.rasterize(key, rgba);
            self.sprites.insert((key, rgba), built);
        }
        self.sprites.get(&(key, rgba)).and_then(|p| p.as_ref())
    }

    fn rasterize(&mut self, key: CacheKey, rgba: u32) -> Option<Pixmap> {
        let (cr, cg, cb, ca) = (
            (rgba >> 24) & 0xff,
            (rgba >> 16) & 0xff,
            (rgba >> 8) & 0xff,
            rgba & 0xff,
        );
        // Copy what we need out of the swash cache to end the borrow.
        let (data, content, w, h) = {
            let image = self.swash.get_image(&mut self.font_system, key).as_ref()?;
            let p = image.placement;
            if p.width <= 0 || p.height <= 0 {
                return None;
            }
            (image.data.clone(), image.content, p.width, p.height)
        };
        let mut out = Pixmap::new(w, h)?;
        let px = out.pixels_mut();
        match content {
            SwashContent::Color => {
                for (i, px) in px.iter_mut().enumerate() {
                    let base = i * 4;
                    if base + 3 >= data.len() {
                        break;
                    }
                    let a = (data[base + 3] as u32 * ca) / 255;
                    *px = tiny_skia::PremultipliedColorU8::from_rgba(
                        (data[base] as u32 * a / 255) as u8,
                        (data[base + 1] as u32 * a / 255) as u8,
                        (data[base + 2] as u32 * a / 255) as u8,
                        a as u8,
                    )?;
                }
            }
            SwashContent::Mask | SwashContent::SubpixelMask => {
                let stride = if content == SwashContent::Mask { 1 } else { 4 };
                for (i, px) in px.iter_mut().enumerate() {
                    let at = i * stride;
                    if at >= data.len() {
                        break;
                    }
                    let a = if stride == 1 {
                        data[at] as u32
                    } else {
                        (data[at] as u32)
                            .max(data[at + 1] as u32)
                            .max(data[at + 2] as u32)
                    };
                    let a = (a * ca) / 255;
                    *px = tiny_skia::PremultipliedColorU8::from_rgba(
                        (cr * a / 255) as u8,
                        (cg * a / 255) as u8,
                        (cb * a / 255) as u8,
                        a as u8,
                    )?;
                }
            }
        }
        Some(out)
    }

    // A glyph run is described by text, size, weight, slant, face, colour,
    // alpha and rotation; grouping those into a struct would not help callers.
    #[allow(clippy::too_many_arguments)]
    /// Draws `text` with its baseline at (`x`, `y`), in physical pixels.
    ///
    /// `rotation` is applied around (`x`, `y`), matching the falling words.
    pub fn draw(
        &mut self,
        dst: &mut Pixmap,
        text: &str,
        x: f32,
        y: f32,
        size: f32,
        bold: bool,
        italic: bool,
        face: Face,
        color: [f64; 3],
        alpha: f64,
        rotation: Option<f32>,
    ) {
        if text.is_empty() || alpha <= 0.004 {
            return;
        }
        let rgba = pack_rgba(color, alpha);
        let laid = self.layout(text, size, bold, italic, face);
        let paint = PixmapPaint {
            quality: FilterQuality::Bilinear,
            ..Default::default()
        };
        let rotate = rotation.unwrap_or(0.0);
        for (key, gx, gy) in &laid.glyphs {
            let Some(sprite) = self.sprite(*key, rgba) else {
                continue;
            };
            // Glyph offsets are relative to the line top; rebase them on the
            // baseline the caller asked for.
            let local_x = *gx as f32;
            let local_y = *gy as f32 - laid.baseline;
            if rotate == 0.0 {
                dst.draw_pixmap(
                    (x + local_x).round() as i32,
                    (y + local_y).round() as i32,
                    sprite.as_ref(),
                    &paint,
                    Transform::identity(),
                    None,
                );
            } else {
                // Map the sprite from its own coordinates: rotate it about the
                // anchor instead of merely shifting it (a shift would shear the
                // word along the baseline).
                let tf = Transform::from_translate(x, y)
                    .pre_concat(Transform::from_rotate(rotate))
                    .pre_concat(Transform::from_translate(local_x, local_y));
                dst.draw_pixmap(0, 0, sprite.as_ref(), &paint, tf, None);
            }
        }
    }
}

fn pack_rgba(color: [f64; 3], alpha: f64) -> u32 {
    let q = |v: f64| ((v.clamp(0.0, 1.0) * 255.0).round() as u32) & 0xff;
    // 32 alpha steps: fades animate smoothly without exploding the cache.
    let a = (((alpha.clamp(0.0, 1.0) * 255.0) / 8.0).round() as u32 * 8).min(255);
    (q(color[0]) << 24) | (q(color[1]) << 16) | (q(color[2]) << 8) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_and_draws_text() {
        let mut t = TextEngine::new();
        let w = t.measure("Hello", 24.0, true, false, Face::Latin);
        assert!(w > 20.0, "width {w}");
        let mut pm = Pixmap::new(200, 60).unwrap();
        t.draw(
            &mut pm,
            "Hello",
            4.0,
            30.0,
            24.0,
            true,
            false,
            Face::Latin,
            [1.0, 1.0, 1.0],
            1.0,
            None,
        );
        assert!(pm.pixels().iter().any(|p| p.alpha() > 0), "nothing drawn");
    }

    #[test]
    fn cjk_uses_a_cjk_face_and_renders() {
        let mut t = TextEngine::new();
        let face = Face::for_text("東京の夜");
        assert_eq!(face, Face::CjkJp);
        let mut pm = Pixmap::new(200, 60).unwrap();
        t.draw(
            &mut pm,
            "東京の夜",
            4.0,
            40.0,
            26.0,
            false,
            false,
            face,
            [1.0, 1.0, 1.0],
            1.0,
            None,
        );
        assert!(pm.pixels().iter().any(|p| p.alpha() > 0));
    }

    #[test]
    fn rotation_moves_pixels_around_the_anchor() {
        let mut t = TextEngine::new();
        let mut pm = Pixmap::new(80, 80).unwrap();
        t.draw(
            &mut pm,
            "||||",
            10.0,
            40.0,
            20.0,
            true,
            false,
            Face::Latin,
            [1.0, 1.0, 1.0],
            1.0,
            Some(std::f32::consts::FRAC_PI_2),
        );
        assert!(pm.pixels().iter().any(|p| p.alpha() > 0));
    }

    #[test]
    fn ellipsize_matches_measure() {
        let mut t = TextEngine::new();
        let cut = t.ellipsize(
            "a very long title indeed",
            60.0,
            14.0,
            true,
            false,
            Face::Latin,
        );
        assert!(cut.ends_with('\u{2026}'));
        assert!(t.measure(&cut, 14.0, true, false, Face::Latin) <= 60.0);
    }
}
