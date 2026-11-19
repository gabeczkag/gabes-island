//! The overlay state machine — the native twin of the Linux `App`.
//!
//! It owns everything platform independent: the media snapshot, the playback
//! clock, the lyric cursor, the spring animation, the random-word widget, the
//! waterfall, edit mode and the render caches.  A [`Overlay`] implementation
//! turns surfaces into real windows; nothing in here knows about HWNDs or
//! NSWindows, which is why it is unit tested on any host.

use std::sync::mpsc;
use std::thread;

use island_core::config::{self, Layout, MAX_BARS, PLACE_BOTH, SOURCE_LYRICS};
use island_core::fall::{self, FallWord, WaterfallRect};
use island_core::geom::{self, Spring};
use island_core::lyrics::ParsedLyrics;
use island_core::media::{Media, PlayerAction};
use island_core::rng::Rng;
use island_core::text::fmt_time;
use island_core::timing::{self, DriftGuard, PlaybackClock, Timeline};
use tiny_skia::Pixmap;

use crate::media::MediaSource;
use crate::net;
use crate::render::{self, PillFrame, WidgetFrame};
use crate::spectrum::SpectrumTracker;
use crate::text::{Face, TextEngine};

/// The four windows the overlay owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Surface {
    /// The now-playing pill.
    Pill,
    /// The 26x26 sliders button at the top of the screen.
    EditButton,
    /// The random word widget (hidden while the waterfall is on).
    Widget,
    /// The falling-words column.
    Waterfall,
}

impl Surface {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn key(self) -> &'static str {
        match self {
            Surface::Pill => "pill",
            Surface::EditButton => "edit",
            Surface::Widget => "widget",
            Surface::Waterfall => "waterfall",
        }
    }
}

/// A platform window set. Geometry is in logical (point) pixels.
pub trait Overlay {
    fn place(&mut self, s: Surface, x: i32, y: i32, w: i32, h: i32);
    fn present(&mut self, s: Surface, pm: &Pixmap);
    fn show(&mut self, s: Surface);
    fn hide(&mut self, s: Surface);
    /// Logical size of the primary display.
    fn screen(&self) -> (i32, i32);
    /// DPI factor the surfaces are rendered at.
    fn scale(&self) -> f32;
    /// A fullscreen app should hide the island (default: unknown = no).
    fn fullscreen(&mut self) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

#[derive(Clone, Debug)]
pub struct WidgetState {
    pub text: String,
    pub w: i32,
    pub h: i32,
    pub x: i32,
    pub y: i32,
    pub start: u128,
    pub dur: u128,
    pub size: f64,
    pub font: usize,
    pub color: [f64; 3],
    pub bold: bool,
    pub italic: bool,
    pub depth: i32,
    pub angle: f64,
}

pub struct FrameParts {
    title: String,
    artist: String,
    time: String,
    progress: Option<f64>,
    anim: f64,
}

pub struct App {
    pub layout: Layout,
    engine: TextEngine,
    pub media: Media,
    pub have_media: bool,
    pub clock: PlaybackClock,
    pub timeline: Timeline,
    drift: DriftGuard,
    pub latency_ms: i64,
    spring: Spring,
    shown: bool,
    pub target_w: i32,
    pub pill_h: i32,
    pub cur_w: i32,
    pub cur_h: i32,
    pub pill_x: i32,
    pub pill_y: i32,
    art: Option<Pixmap>,
    art_key: String,
    pub levels: [f32; MAX_BARS],
    rng: Rng,
    words: Vec<String>,
    lyrics: ParsedLyrics,
    lyrics_req: String,
    lyrics_tx: mpsc::Sender<(String, String)>,
    lyrics_rx: mpsc::Receiver<(String, String)>,
    art_tx: mpsc::Sender<(String, String)>,
    art_rx: mpsc::Receiver<(String, String)>,
    widget: Option<WidgetState>,
    widget_next: u128,
    widget_placed_y: i32,
    plain_idx: usize,
    last_word: String,
    display_ready: bool,
    fall_words: Vec<FallWord>,
    fall_next: u128,
    fall_last_tick: u128,
    fall_last_draw: u128,
    fall_rect: Option<WaterfallRect>,
    fall_mapped: bool,
    pub edit_mode: bool,
    pub edit_sel: i32,
    pub dragging: bool,
    drag: Option<(f64, f64, i32, i32)>,
    pub hover_btn: i32,
    edit_hover: bool,
    pill_static: Option<Pixmap>,
    pill_frame: Option<Pixmap>,
    static_key: u64,
    widget_pm: Option<Pixmap>,
    fall_pm: Option<Pixmap>,
    edit_pm: Option<Pixmap>,
    edit_pm_key: u64,
    pill_dirty: bool,
    widget_dirty: bool,
    last_pill_draw: u128,
    last_widget_draw: u128,
    last_tick: u128,
    last_poll: u128,
    fullscreen: bool,
    scale: f32,
    screen_w: i32,
    screen_h: i32,
}

impl App {
    pub fn new(layout: Layout, scale: f32, screen: (i32, i32), mock: bool) -> App {
        let (lyrics_tx, lyrics_rx) = mpsc::channel();
        let (art_tx, art_rx) = mpsc::channel();
        let words = config::load_words(&config::words_path());
        let _ = mock;
        let mut app = App {
            target_w: layout_initial_width(&layout),
            pill_h: layout.height.max(layout.min_pill_height()),
            cur_w: 48,
            cur_h: 7,
            pill_x: (screen.0 - 48) / 2 + layout.xoff,
            pill_y: layout.top,
            layout,
            engine: TextEngine::new(),
            media: Media::default(),
            have_media: false,
            clock: PlaybackClock::default(),
            timeline: Timeline::default(),
            drift: DriftGuard::default(),
            latency_ms: 0,
            spring: Spring::default(),
            shown: false,
            art: None,
            art_key: String::new(),
            levels: [0.0; MAX_BARS],
            rng: Rng::from_time(),
            words,
            lyrics: ParsedLyrics::default(),
            lyrics_req: String::new(),
            lyrics_tx,
            lyrics_rx,
            art_tx,
            art_rx,
            widget: None,
            widget_next: 8000,
            widget_placed_y: 0,
            plain_idx: 0,
            last_word: String::new(),
            display_ready: true,
            fall_words: Vec::new(),
            fall_next: 0,
            fall_last_tick: 0,
            fall_last_draw: 0,
            fall_rect: None,
            fall_mapped: false,
            edit_mode: false,
            edit_sel: -1,
            dragging: false,
            drag: None,
            hover_btn: -1,
            edit_hover: false,
            pill_static: None,
            pill_frame: None,
            static_key: 0,
            widget_pm: None,
            fall_pm: None,
            edit_pm: None,
            edit_pm_key: 0,
            pill_dirty: true,
            widget_dirty: true,
            last_pill_draw: 0,
            last_widget_draw: 0,
            last_tick: 0,
            last_poll: u128::MAX,
            fullscreen: false,
            scale,
            screen_w: screen.0,
            screen_h: screen.1,
        };
        app.target_w = app.measure_target();
        app.pill_h = app.layout.height.max(app.layout.min_pill_height());
        app
    }

    // ------------------------------------------------------------ properties

    /// Reconfigures for the real screen/DPI once the first window exists.
    pub fn set_screen(&mut self, sw: i32, sh: i32, scale: f32) {
        if self.screen_w == sw && self.screen_h == sh && (self.scale - scale).abs() < 0.001 {
            return;
        }
        self.screen_w = sw;
        self.screen_h = sh;
        self.scale = scale;
        self.pill_static = None;
        self.pill_frame = None;
        self.widget_pm = None;
        self.fall_pm = None;
        self.edit_pm = None;
        self.static_key = 0;
        self.fall_rect = None;
        self.target_w = self.measure_target();
        self.pill_x = (sw - self.cur_w) / 2 + self.layout.xoff;
        self.pill_dirty = true;
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn shown(&self) -> bool {
        self.shown
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn widget(&self) -> Option<&WidgetState> {
        self.widget.as_ref()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn fall_words(&self) -> &[FallWord] {
        &self.fall_words
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn art(&self) -> Option<&Pixmap> {
        self.art.as_ref()
    }

    pub fn current_pos_ms(&self, now: u128) -> i64 {
        self.clock.position_ms(now)
    }

    pub fn sync_pos_ms(&self, now: u128) -> i64 {
        self.clock.sync_position_ms(now, self.latency_ms)
    }

    fn playing(&self) -> bool {
        self.media.playing == 1
    }

    // ------------------------------------------------------------ lyrics/art

    /// Called on every poll: reacts to a track change, cover change and
    /// lyric request exactly like the Linux poll loop.
    pub fn apply_media(&mut self, now: u128, fresh: Media) {
        let changed = fresh.title != self.media.title
            || fresh.artist != self.media.artist
            || fresh.art_key() != self.media.art_key();
        self.have_media = true;
        self.clock.set(fresh.pos_us, now, fresh.playing);
        if fresh.art_key() != self.art_key {
            self.art_key = fresh.art_key();
            self.load_art(&fresh);
            self.pill_dirty = true;
            self.static_key = 0;
        }
        if changed {
            self.media = fresh;
            self.target_w = self.measure_target();
            self.latency_ms = timing::resolve_latency(self.layout.latency, false);
            self.pill_dirty = true;
            self.static_key = 0;
        } else {
            self.media.playing = fresh.playing;
            self.media.length_us = fresh.length_us;
            self.media.pos_us = fresh.pos_us;
            self.media.player = fresh.player;
        }
        if self.layout.wsource == SOURCE_LYRICS {
            let key = self.media.track_key();
            if !self.media.title.is_empty() && self.lyrics_req != key {
                self.request_lyrics(key);
            }
        }
    }

    fn load_art(&mut self, m: &Media) {
        if let Some(bytes) = &m.cover {
            self.art = decode_cover(bytes);
            return;
        }
        if m.art.is_empty() {
            self.art = None;
            return;
        }
        if let Some(path) = net::file_path_from_url(&m.art) {
            self.art = net::load_cover(std::path::Path::new(&path));
        } else if m.art.starts_with("http") {
            let tx = self.art_tx.clone();
            let url = m.art.clone();
            thread::spawn(move || {
                if let Some(path) = net::download_art(&url) {
                    let _ = tx.send((url, path));
                }
            });
        } else {
            self.art = net::load_cover(std::path::Path::new(&m.art));
        }
    }

    fn request_lyrics(&mut self, key: String) {
        self.lyrics_req = key.clone();
        self.lyrics = ParsedLyrics::default();
        self.timeline.clear();
        self.plain_idx = 0;
        self.last_word.clear();
        self.fall_words.clear();
        self.fall_next = 0;
        self.widget = None;
        self.widget_next = 0;
        self.display_ready = self.layout.warmup == 0;

        let (artist, title) = split_key(&key);
        if let Some(cached) = net::cached_lyrics(&artist, &title) {
            self.accept_lyrics(&key, &cached);
            return;
        }
        let tx = self.lyrics_tx.clone();
        thread::spawn(move || {
            if let Some(text) = net::fetch_lyrics(&artist, &title) {
                let _ = tx.send((key, text));
            }
        });
    }

    fn accept_lyrics(&mut self, key: &str, text: &str) {
        if key != self.lyrics_req {
            return;
        }
        let parsed = ParsedLyrics::from_payload(text, self.layout.wsync != 0, self.layout.wbpm);
        self.lyrics = parsed;
        self.timeline.reset(self.lyrics.timed.clone());
        self.plain_idx = 0;
        self.last_word.clear();
        self.widget = None;
        self.widget_dirty = true;
        self.fall_words.clear();
        self.display_ready = self.layout.warmup == 0;
        self.drift.reset();
        if let Some((artist, title)) = self.lyrics_req.split_once('|') {
            net::cache_lyrics(artist, title, text);
        }
    }

    fn drain_channels(&mut self, now: u128) {
        while let Ok((key, text)) = self.lyrics_rx.try_recv() {
            self.accept_lyrics(&key, &text);
            let _ = now;
        }
        while let Ok((url, path)) = self.art_rx.try_recv() {
            if url == self.art_key {
                self.art = net::load_cover(std::path::Path::new(&path));
                self.static_key = 0;
                self.pill_dirty = true;
            }
        }
    }

    // --------------------------------------------------------------- update

    /// Advances all state; call once per event-loop iteration.
    pub fn update(
        &mut self,
        now: u128,
        media_src: &mut dyn MediaSource,
        spectrum: &mut SpectrumTracker,
        fullscreen: bool,
    ) {
        self.fullscreen = fullscreen;
        self.drain_channels(now);

        let interval = if self.playing() { 500 } else { 1000 };
        if self.last_poll == u128::MAX || now.saturating_sub(self.last_poll) >= interval {
            self.last_poll = now;
            match media_src.poll() {
                Some(fresh) => self.apply_media(now, fresh),
                None => {
                    if self.have_media {
                        self.have_media = false;
                        self.pill_dirty = true;
                        self.static_key = 0;
                    }
                    self.media.playing = 0;
                }
            }
        }

        self.levels = spectrum.tick(self.playing(), now);
        if self.layout.latency < 0 {
            self.latency_ms = timing::resolve_latency(self.layout.latency, spectrum.bluetooth);
        }

        if !self.display_ready && self.clock.position_ms(now) >= self.layout.warmup as i64 {
            self.display_ready = true;
        }

        self.update_widget(now);
        self.update_fall(now);
        self.update_visibility(now);
    }

    fn update_visibility(&mut self, now: u128) {
        let _ = now;
        let want_show = self.edit_mode || (self.have_media && !self.fullscreen && self.playing());

        if want_show {
            if !self.shown {
                self.cur_w = 48;
                self.cur_h = 7;
                self.spring.value = 0.0;
                self.spring.show();
                self.shown = true;
                self.pill_dirty = true;
                self.static_key = 0;
            } else if !self.spring.expanding {
                // came back while retracting (or edit mode was toggled)
                self.spring.show();
            }
        } else if self.shown && !self.spring.fully_hidden() {
            self.spring.hide();
        }

        if self.shown {
            if self.spring.animating {
                if now.saturating_sub(self.last_tick) >= 16 {
                    let dt =
                        (now.saturating_sub(self.last_tick) as f64 / 1000.0).clamp(0.001, 0.05);
                    self.spring.step(dt, self.layout.anim);
                    self.last_tick = now;
                    if self.edit_mode {
                        self.cur_w = self.target_w;
                        self.cur_h = self.pill_h;
                    } else {
                        let (w, h) = self.spring.pill_size(self.target_w, self.pill_h);
                        self.cur_w = w;
                        self.cur_h = h;
                    }
                    self.pill_x = (self.screen_w - self.cur_w) / 2 + self.layout.xoff;
                    self.pill_y = self.layout.top;
                    self.pill_dirty = true;
                }
            } else if self.spring.fully_hidden() {
                self.shown = false;
                self.pill_dirty = true;
            }
        }
    }

    fn pool_ready(&self) -> bool {
        if self.layout.wsource == SOURCE_LYRICS {
            !self.lyrics.words.is_empty() && self.playing()
        } else {
            !self.words.is_empty()
        }
    }

    fn pool(&self) -> Vec<String> {
        if self.layout.wsource == SOURCE_LYRICS {
            self.lyrics.words.clone()
        } else {
            self.words.clone()
        }
    }

    fn widget_phase(&self, now: u128) -> f64 {
        let Some(w) = &self.widget else { return 1.0 };
        (now.saturating_sub(w.start) as f64 / w.dur.max(1) as f64).clamp(0.0, 1.0)
    }

    fn update_widget(&mut self, now: u128) {
        if self.layout.widgets == 0 {
            self.widget = None;
            return;
        }
        if self.layout.wfall != 0 {
            self.widget = None;
            return;
        }
        if !self.display_ready {
            self.widget = None;
            return;
        }
        let synced = self.layout.wcont != 0
            && self.layout.wsync != 0
            && self.lyrics.synced
            && !self.lyrics.timed.is_empty();
        if synced {
            self.update_synced(now);
            return;
        }
        match self.widget.take() {
            None => {
                let continuous = self.layout.wcont != 0;
                if continuous {
                    if self.pool_ready() {
                        self.show_widget(now);
                    }
                } else if now >= self.widget_next {
                    self.show_widget(now);
                }
            }
            Some(w) => {
                let phase =
                    (now.saturating_sub(w.start) as f64 / w.dur.max(1) as f64).clamp(0.0, 1.0);
                if phase >= 1.0 {
                    if self.layout.wcont != 0 {
                        if self.pool_ready() {
                            self.show_widget(now);
                        }
                    } else {
                        let span = self.layout.wmax.saturating_sub(self.layout.wmin) + 1;
                        let secs = self.layout.wmin + self.rng.below(span.max(1));
                        self.widget_next = now + secs as u128 * 1000;
                    }
                } else {
                    self.widget = Some(w);
                    self.widget_dirty = true;
                    self.move_widget(now, phase);
                }
            }
        }
    }

    fn update_synced(&mut self, now: u128) {
        if !self.playing() {
            self.widget = None;
            return;
        }
        if let Some(w) = &self.widget {
            let phase = self.widget_phase(now);
            if now.saturating_sub(w.start) >= w.dur {
                self.widget = None;
                self.widget_dirty = true;
            } else {
                self.widget_dirty = true;
                self.move_widget(now, phase);
            }
        }
        let pos = self.sync_pos_ms(now);
        if let Some((start, end)) = self.timeline.advance_to(pos) {
            if let Some((_, word)) = self.timeline.words.get(end - 1).cloned() {
                let dur = self.timeline.display_duration(
                    end - 1,
                    pos,
                    self.layout.wms,
                    self.rng.next_u64(),
                );
                let _ = start;
                self.show_word(word, now, dur);
            }
        }
    }

    fn move_widget(&mut self, _now: u128, phase: f64) {
        if let Some(w) = &mut self.widget {
            self.widget_placed_y = w.y - (phase * 8.0) as i32;
        }
    }

    fn show_widget(&mut self, now: u128) {
        let pool = self.pool();
        if pool.is_empty() {
            return;
        }
        let word = if self.layout.wcont != 0 {
            let w = pool[self.plain_idx % pool.len()].clone();
            self.plain_idx = (self.plain_idx + 1) % pool.len().max(1);
            w
        } else {
            let mut idx = self.rng.below(pool.len() as u64) as usize;
            if pool.len() > 1 && pool[idx] == self.last_word {
                idx = (idx + 1) % pool.len();
            }
            pool[idx].clone()
        };
        self.last_word = word.clone();
        let dur = if self.layout.wcont != 0 {
            (250 + self.rng.below(251)).min(self.layout.wms) as u128
        } else {
            3200
        };
        self.show_word(word, now, dur);
    }

    /// Random typography + placement for one word (Linux `show_word`).
    fn show_word(&mut self, word: String, now: u128, dur: u128) {
        let r1 = self.rng.next_u64();
        let r2 = self.rng.next_u64();
        let r3 = self.rng.next_u64();
        let font = if r1.is_multiple_of(4) { 1 } else { 0 };
        let size = 18.0 + (r2 % 9) as f64;
        let bold = !r3.is_multiple_of(5);
        let italic = (r3 >> 3).is_multiple_of(4);
        let color = fall::WIDGET_COLORS[(r3 >> 6) as usize % fall::WIDGET_COLORS.len()];
        let depth = 2 + (r1 % 4) as i32;
        let angle = ((r2 % 1000) as f64 / 1000.0 - 0.5) * 0.24;

        let face = Face::for_text(&word);
        let fs = size.clamp(12.0, 26.0);
        let tw = self.engine.measure(&word, fs as f32, bold, italic, face) as f64;
        let pad = 34 + if self.layout.w3d != 0 { depth * 3 } else { 0 };
        let h = (fs * 2.2 + 16.0) as i32;
        let w = ((tw as i32) + pad).clamp(70, (self.screen_w - 40).max(70));

        let place = if self.layout.wplace >= PLACE_BOTH {
            (self.rng.below(2)) as i32
        } else {
            self.layout.wplace
        };
        let (x, y) = if place == 1 {
            let island_x = (self.screen_w - self.cur_w) / 2 + self.layout.xoff;
            let cx = island_x + self.cur_w / 2;
            let px = (cx - w / 2).clamp(6, (self.screen_w - w - 6).max(6));
            let mut py = self.layout.top - h - 8;
            if py < 4 {
                py = self.layout.top + self.pill_h + 8;
            }
            (px, py.clamp(4, (self.screen_h - h - 4).max(4)))
        } else {
            let x_range = (self.screen_w - w - 40).max(1) as u64;
            let y_range = (self.screen_h - h - 160).max(1) as u64;
            (
                20 + self.rng.below(x_range) as i32,
                100 + self.rng.below(y_range) as i32,
            )
        };

        self.widget_placed_y = y;
        self.widget = Some(WidgetState {
            text: word,
            w,
            h,
            x,
            y,
            start: now,
            dur,
            size,
            font,
            color,
            bold,
            italic,
            depth,
            angle,
        });
        self.widget_dirty = true;
        self.last_widget_draw = 0;
    }

    // ------------------------------------------------------------ waterfall

    pub fn waterfall_rect(&self) -> WaterfallRect {
        let island_x = (self.screen_w - self.cur_w) / 2 + self.layout.xoff;
        fall::waterfall_rect(
            self.screen_w,
            self.screen_h,
            island_x,
            self.cur_w,
            self.layout.top,
            self.pill_h,
        )
    }

    fn update_fall(&mut self, now: u128) {
        if self.layout.wfall == 0 {
            if self.fall_mapped {
                self.fall_mapped = false;
            }
            self.fall_words.clear();
            return;
        }
        let ready = self.display_ready
            && self.playing()
            && if self.layout.wsource == SOURCE_LYRICS {
                !self.lyrics.words.is_empty()
            } else {
                !self.words.is_empty()
            };
        if !ready {
            if self.fall_mapped {
                self.fall_mapped = false;
            }
            self.fall_words.clear();
            self.fall_rect = None;
            return;
        }
        let rect = self.waterfall_rect();
        if self.fall_rect != Some(rect) {
            self.fall_rect = Some(rect);
            self.fall_words.clear();
            self.fall_last_tick = now;
            self.fall_mapped = true;
        }

        if self.layout.wsync != 0 && self.lyrics.synced && !self.lyrics.timed.is_empty() {
            let pos = self.sync_pos_ms(now);
            if let Some((start, end)) = self.timeline.advance_to(pos) {
                for i in start..end {
                    let word = self.timeline.words[i].1.clone();
                    self.spawn_fall_word(word, now);
                }
                let _ = end;
            }
        } else if now >= self.fall_next {
            let pool = self.pool();
            if !pool.is_empty() {
                let word = pool[self.plain_idx % pool.len()].clone();
                self.plain_idx = (self.plain_idx + 1) % pool.len().max(1);
                self.spawn_fall_word(word, now);
                self.fall_next = now + 250 + self.rng.below(251) as u128;
            }
        }

        if now.saturating_sub(self.fall_last_draw) < 50 {
            return;
        }
        self.fall_last_draw = now;
        let dt = if self.fall_last_tick == 0 {
            0.0
        } else {
            ((now.saturating_sub(self.fall_last_tick)) as f64 / 1000.0).min(0.1)
        };
        self.fall_last_tick = now;
        for w in &mut self.fall_words {
            fall::integrate(w, dt);
        }
        let screen_h = self.screen_h;
        self.fall_words.retain(|w| !fall::expired(w, now, screen_h));
    }

    fn spawn_fall_word(&mut self, word: String, now: u128) {
        let island_x = (self.screen_w - self.cur_w) / 2 + self.layout.xoff;
        let fall_x0 = self.fall_rect.map(|r| r.x).unwrap_or(0);
        let (x, y) = fall::spawn_origin(island_x, self.cur_w, fall_x0, &mut self.rng);
        if self.fall_words.len() >= fall::MAX_FALL_WORDS {
            self.fall_words.remove(0);
        }
        self.fall_words
            .push(fall::spawn(word, x, y, now, &mut self.rng));
    }

    /// How long the loop may sleep before the next frame is useful.
    ///
    /// Idle (paused, nothing floating) wakes four times a second instead of
    /// sixty: the pill is static and nothing on screen moves.
    pub fn next_wake_ms(&self) -> u64 {
        if self.shown && self.spring.animating {
            16
        } else if self.widget.is_some() {
            33
        } else if self.fall_mapped || (self.shown && self.playing()) {
            50
        } else {
            250
        }
    }

    // -------------------------------------------------------------- geometry

    /// Width the pill wants for the current title/artist.
    pub fn measure_target(&mut self) -> i32 {
        let title = if self.media.title.is_empty() {
            "Edytujesz wyspę"
        } else {
            self.media.title.as_str()
        };
        let artist = if self.media.artist.is_empty() {
            "przeciągnij • kółko"
        } else {
            self.media.artist.as_str()
        };
        let ts = (self.layout.text_h / 26.0).clamp(0.6, 2.0);
        let tface = Face::for_text(title);
        let t = self
            .engine
            .ellipsize(title, 460.0, (15.0 * ts) as f32, true, false, tface);
        let tw = self
            .engine
            .measure(&t, (15.0 * ts) as f32, true, false, tface) as f64;
        let aface = Face::for_text(artist);
        let a = self
            .engine
            .ellipsize(artist, 460.0, (12.0 * ts) as f32, false, false, aface);
        let aw = self
            .engine
            .measure(&a, (12.0 * ts) as f32, false, false, aface) as f64;
        geom::pill_width(&self.layout, tw.max(aw))
    }

    /// Text content for the pill, owned so the frame can borrow `layout` and
    /// `art` independently of `engine` while drawing.
    fn frame_parts(&self, now: u128) -> FrameParts {
        let artist = if self.edit_mode {
            self.edit_hint()
        } else {
            self.media.artist.clone()
        };
        let title = if self.media.title.is_empty() {
            if self.edit_mode {
                "Edytujesz wyspę".to_string()
            } else {
                "Now playing".to_string()
            }
        } else {
            self.media.title.clone()
        };
        let pos = self.current_pos_ms(now);
        let progress = if self.media.length_us > 0 {
            Some((pos as f64 / self.media.length_us as f64).clamp(0.0, 1.0))
        } else {
            None
        };
        FrameParts {
            title,
            artist,
            time: fmt_time(pos * 1000),
            progress,
            anim: if self.edit_mode {
                1.0
            } else {
                self.spring.value
            },
        }
    }

    fn edit_hint(&self) -> String {
        let anim = self.layout.anim;
        match self.edit_sel {
            0 => format!("anim {anim:.1}s • okładka • kółko = szer. • Alt = wys."),
            1 => format!("anim {anim:.1}s • tekst • kółko = szer. • Alt = wys."),
            2 => format!("anim {anim:.1}s • czas • kółko = szer. • Alt = wys."),
            3 => format!("anim {anim:.1}s • fale • kółko = szer. • Alt = wys."),
            4 => format!("anim {anim:.1}s • przyciski • kółko = szer. • Alt = wys."),
            5 => {
                format!("anim {anim:.1}s • główny box • kółko = wys. • Shift = szer. • Alt = wys.")
            }
            _ => format!(
                "anim {anim:.1}s • wybierz box • kółko = wys. • Shift = szer. • Ctrl = anim"
            ),
        }
    }

    fn static_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.cur_w.hash(&mut h);
        self.cur_h.hash(&mut h);
        self.pill_h.hash(&mut h);
        let l = &self.layout;
        for f in [
            l.art, l.art_h, l.text_h, l.time, l.time_h, l.spec, l.spec_h, l.btn, l.btn_h, l.gap,
            l.padx, l.extra,
        ] {
            f.to_bits().hash(&mut h);
        }
        let anim_bits = (if self.edit_mode {
            1.0
        } else {
            self.spring.value
        } * 1000.0) as i64;
        anim_bits.hash(&mut h);
        self.edit_mode.hash(&mut h);
        self.edit_sel.hash(&mut h);
        self.hover_btn.hash(&mut h);
        self.media.playing.hash(&mut h);
        self.media.title.hash(&mut h);
        self.media.artist.hash(&mut h);
        self.art_key.hash(&mut h);
        self.art.is_some().hash(&mut h);
        h.finish()
    }

    // ------------------------------------------------------------------ draw

    /// Presents every dirty surface. Cheap when nothing changed.
    pub fn draw(&mut self, now: u128, ov: &mut dyn Overlay) {
        let (sw, sh) = ov.screen();
        if (sw, sh) != (self.screen_w, self.screen_h) {
            self.screen_w = sw;
            self.screen_h = sh;
            self.target_w = self.measure_target();
            self.fall_rect = None;
        }
        let scale = ov.scale();
        if (scale - self.scale).abs() > 0.01 {
            self.scale = scale;
            self.pill_static = None;
            self.pill_frame = None;
            self.widget_pm = None;
            self.fall_pm = None;
            self.edit_pm = None;
            self.static_key = 0;
        }

        // the sliders button: always present, redrawn only on hover changes
        let edit_key = (self.edit_mode as u64) << 1 | self.edit_hover as u64;
        if self.edit_pm.is_none() || edit_key != self.edit_pm_key || self.edit_pm.is_none() {
            self.edit_pm = render::render_edit_button(
                &mut self.engine,
                self.edit_mode,
                self.edit_hover,
                self.scale,
            );
            self.edit_pm_key = edit_key;
            if let Some(pm) = &self.edit_pm {
                ov.place(Surface::EditButton, self.screen_w / 2 - 13, 12, 26, 26);
                ov.present(Surface::EditButton, pm);
                ov.show(Surface::EditButton);
            }
        }

        self.draw_pill(now, ov);
        self.draw_widget(now, ov);
        self.draw_waterfall(now, ov);
    }

    fn draw_pill(&mut self, now: u128, ov: &mut dyn Overlay) {
        if !self.shown {
            ov.hide(Surface::Pill);
            self.pill_dirty = false;
            return;
        }
        let key = self.static_key();
        let static_changed = key != self.static_key;
        let dynamic_due = self.playing() && now.saturating_sub(self.last_pill_draw) >= 50;
        if !(static_changed || dynamic_due || self.pill_dirty) {
            return;
        }
        self.last_pill_draw = now;

        let parts = self.frame_parts(now);
        let playing = self.playing();
        if static_changed || self.pill_static.is_none() {
            self.pill_static =
                render::pill_pixmap(self.cur_w as f64, self.cur_h as f64, self.scale);
            if let Some(pm) = &mut self.pill_static {
                let f = PillFrame {
                    layout: &self.layout,
                    width: self.cur_w as f64,
                    height: self.cur_h as f64,
                    anim: parts.anim,
                    title: parts.title.clone(),
                    artist: parts.artist.clone(),
                    time: parts.time.clone(),
                    progress: parts.progress,
                    levels: self.levels,
                    playing,
                    art: self.art.as_ref(),
                    hover_btn: self.hover_btn,
                    edit_mode: self.edit_mode,
                    edit_sel: self.edit_sel,
                };
                render::draw_pill_static(pm, &mut self.engine, &f, self.scale);
            }
            self.static_key = key;
        }
        let Some(static_pm) = &self.pill_static else {
            return;
        };
        let (pw, ph) = (static_pm.width(), static_pm.height());
        if self
            .pill_frame
            .as_ref()
            .map(|p| (p.width(), p.height()) != (pw, ph))
            .unwrap_or(true)
        {
            self.pill_frame = Pixmap::new(pw, ph);
        }
        let Some(frame) = &mut self.pill_frame else {
            return;
        };
        frame.data_mut().copy_from_slice(static_pm.data());
        {
            let f = PillFrame {
                layout: &self.layout,
                width: self.cur_w as f64,
                height: self.cur_h as f64,
                anim: parts.anim,
                title: parts.title.clone(),
                artist: parts.artist.clone(),
                time: parts.time.clone(),
                progress: parts.progress,
                levels: self.levels,
                playing,
                art: self.art.as_ref(),
                hover_btn: self.hover_btn,
                edit_mode: self.edit_mode,
                edit_sel: self.edit_sel,
            };
            render::draw_pill_dynamic(frame, &mut self.engine, &f, self.scale);
        }
        ov.place(
            Surface::Pill,
            self.pill_x,
            self.pill_y,
            self.cur_w,
            self.cur_h,
        );
        ov.present(Surface::Pill, frame);
        ov.show(Surface::Pill);
        self.pill_dirty = false;
    }

    fn draw_widget(&mut self, now: u128, ov: &mut dyn Overlay) {
        let Some(w) = self.widget.clone() else {
            ov.hide(Surface::Widget);
            return;
        };
        let phase = self.widget_phase(now);
        let due = self.widget_dirty || now.saturating_sub(self.last_widget_draw) >= 33;
        if !due {
            return;
        }
        self.last_widget_draw = now;
        self.widget_dirty = false;

        let f = WidgetFrame {
            text: &w.text,
            width: w.w as f64,
            height: w.h as f64,
            size: w.size,
            font: w.font,
            color: w.color,
            bold: w.bold,
            italic: w.italic,
            depth: w.depth,
            angle: w.angle,
            phase,
            three_d: self.layout.w3d != 0,
        };
        let pm = render::render_widget(&mut self.engine, &f, self.scale);
        if let Some(pm) = &pm {
            let y = w.y - (phase * 8.0) as i32;
            ov.place(Surface::Widget, w.x, y, w.w, w.h);
            ov.present(Surface::Widget, pm);
            ov.show(Surface::Widget);
        }
    }

    fn draw_waterfall(&mut self, now: u128, ov: &mut dyn Overlay) {
        let Some(rect) = self.fall_rect else {
            ov.hide(Surface::Waterfall);
            return;
        };
        let pf = self.playing();
        if !self.fall_mapped || (!pf && self.fall_words.is_empty()) {
            ov.hide(Surface::Waterfall);
            return;
        }
        let (pw, ph) = (
            ((rect.w as f64 * self.scale as f64).round() as u32).max(1),
            ((rect.h as f64 * self.scale as f64).round() as u32).max(1),
        );
        if self
            .fall_pm
            .as_ref()
            .map(|p| (p.width(), p.height()) != (pw, ph))
            .unwrap_or(true)
        {
            self.fall_pm = Pixmap::new(pw, ph);
        }
        let Some(pm) = &mut self.fall_pm else { return };
        if self.fall_words.is_empty() && !pf {
            ov.hide(Surface::Waterfall);
            return;
        }
        render::draw_waterfall(
            pm,
            &mut self.engine,
            &self.fall_words,
            self.layout.wtrail != 0,
            now,
            self.scale,
        );
        ov.place(Surface::Waterfall, rect.x, rect.y, rect.w, rect.h);
        ov.present(Surface::Waterfall, pm);
        ov.show(Surface::Waterfall);
    }

    // ----------------------------------------------------------------- input

    /// Cursor moved on `surface`. `global` is in screen coordinates.
    pub fn on_cursor_move(&mut self, surface: Surface, x: f64, y: f64, global: (f64, f64)) {
        match surface {
            Surface::EditButton => {
                if !self.edit_hover {
                    self.edit_hover = true;
                }
            }
            Surface::Pill => {
                if self.edit_mode && self.dragging {
                    if let Some((sx, sy, ox, oy)) = self.drag {
                        let dx = global.0 - sx;
                        let dy = global.1 - sy;
                        self.layout.xoff = ox + dx as i32;
                        self.layout.top = oy + dy as i32;
                        let lim = (self.screen_w - self.cur_w) / 2;
                        self.layout.xoff = self.layout.xoff.clamp(-lim, lim);
                        self.layout.top = self
                            .layout
                            .top
                            .clamp(0, (self.screen_h - self.cur_h).max(0));
                        self.pill_x = (self.screen_w - self.cur_w) / 2 + self.layout.xoff;
                        self.pill_y = self.layout.top;
                        self.pill_dirty = true;
                    }
                } else if !self.edit_mode {
                    let hover = button_at(&self.layout, self.cur_w as f64, self.cur_h as f64, x, y);
                    if hover != self.hover_btn {
                        self.hover_btn = hover;
                        self.static_key = 0;
                        self.pill_dirty = true;
                    }
                }
            }
            _ => {}
        }
    }

    pub fn on_cursor_leave(&mut self, surface: Surface) {
        match surface {
            Surface::EditButton => self.edit_hover = false,
            Surface::Pill if !self.edit_mode && self.hover_btn != -1 => {
                self.hover_btn = -1;
                self.static_key = 0;
                self.pill_dirty = true;
            }
            _ => {}
        }
    }

    /// Returns `true` when the click was consumed by the island (the platform
    /// may then skip its own default handling).
    #[allow(clippy::too_many_arguments)]
    pub fn on_button(
        &mut self,
        surface: Surface,
        button: u8,
        pressed: bool,
        mods: Mods,
        local: (f64, f64),
        global: (f64, f64),
        media_src: &mut dyn MediaSource,
    ) {
        let _ = mods;
        match surface {
            Surface::EditButton => {
                if button == 1 && pressed {
                    let on = !self.edit_mode;
                    self.set_edit_mode(on);
                }
            }
            Surface::Pill => {
                if !pressed {
                    if button == 1 && self.edit_mode && self.dragging {
                        self.dragging = false;
                        self.drag = None;
                        self.save_config();
                    }
                    return;
                }
                let bx = self.cur_w as f64;
                let by = self.cur_h as f64;
                let (x, y) = local;
                if button == 2 {
                    let on = !self.edit_mode;
                    self.set_edit_mode(on);
                } else if self.edit_mode {
                    if button == 1 {
                        let er = geom::button_rects(&self.layout, bx, by)[3];
                        if hit(er, x, y) {
                            self.set_edit_mode(false);
                        } else {
                            let boxes = geom::element_boxes(&self.layout, bx, by);
                            let mut sel = -1;
                            for (i, b) in boxes.iter().enumerate() {
                                if hit(*b, x, y) {
                                    sel = i as i32;
                                }
                            }
                            if sel >= 0 {
                                self.edit_sel = sel;
                            } else {
                                let edge = 8.0;
                                let ring = (0.0..=bx).contains(&x)
                                    && (0.0..=by).contains(&y)
                                    && (x < edge || x > bx - edge || y < 5.0 || y > by - 5.0);
                                self.edit_sel = if ring { 5 } else { -1 };
                                if !ring {
                                    self.dragging = true;
                                    self.drag = Some((
                                        global.0,
                                        global.1,
                                        self.layout.xoff,
                                        self.layout.top,
                                    ));
                                }
                            }
                            self.pill_dirty = true;
                            self.static_key = 0;
                        }
                    }
                } else {
                    let rects = geom::button_rects(&self.layout, bx, by);
                    let mut found = None;
                    for (i, r) in rects.iter().enumerate() {
                        if hit(*r, x, y) {
                            found = Some(i);
                        }
                    }
                    match found {
                        Some(3) => self.set_edit_mode(!self.edit_mode),
                        Some(0) => media_src.action(PlayerAction::Previous),
                        Some(1) => media_src.action(PlayerAction::PlayPause),
                        Some(2) => media_src.action(PlayerAction::Next),
                        _ if button == 1 => media_src.action(PlayerAction::PlayPause),
                        _ if button == 3 => media_src.action(PlayerAction::Next),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    pub fn on_wheel(&mut self, surface: Surface, up: bool, mods: Mods) {
        if surface != Surface::Pill || !self.edit_mode {
            return;
        }
        let l = &mut self.layout;
        let d = if up { 2.0 } else { -2.0 };
        if mods.ctrl {
            l.anim = (l.anim + if up { 0.1 } else { -0.1 }).clamp(0.2, 5.0);
        } else if mods.alt {
            if (0..=4).contains(&self.edit_sel) {
                match self.edit_sel {
                    0 => {
                        l.art_h = (l.art_h + d).clamp(18.0, 120.0);
                        self.pill_h = self.pill_h.max((l.art_h + 6.0) as i32);
                    }
                    1 => l.text_h = (l.text_h + d).clamp(16.0, 90.0),
                    2 => l.time_h = (l.time_h + d).clamp(10.0, 70.0),
                    3 => l.spec_h = (l.spec_h + d).clamp(10.0, 90.0),
                    _ => l.btn_h = (l.btn_h + d).clamp(10.0, 48.0),
                }
            } else {
                let min_h = l.min_pill_height();
                self.pill_h = (self.pill_h + if up { 2 } else { -2 }).clamp(min_h, 120);
            }
        } else if self.edit_sel == 5 || self.edit_sel < 0 {
            if mods.shift {
                l.padx = (l.padx + d * 2.0).clamp(0.0, 300.0);
            } else {
                let min_h = (l.art + 6.0).max(30.0) as i32;
                self.pill_h = (self.pill_h + if up { 2 } else { -2 }).clamp(min_h, 120);
            }
        } else {
            match self.edit_sel {
                0 => {
                    l.art = (l.art + d).clamp(18.0, 96.0);
                    self.pill_h = self.pill_h.max((l.art + 6.0) as i32);
                }
                1 => l.extra = (l.extra + d * 3.0).clamp(0.0, 600.0),
                2 => l.time = (l.time + d).clamp(12.0, 140.0),
                3 => l.spec = (l.spec + d).clamp(10.0, 140.0),
                4 => l.btn = (l.btn + d).clamp(10.0, 36.0),
                _ => {}
            }
        }
        self.pill_h = self.pill_h.max(self.layout.min_pill_height());
        self.target_w = self.measure_target();
        self.cur_w = self.target_w;
        self.cur_h = self.pill_h;
        self.pill_x = (self.screen_w - self.cur_w) / 2 + self.layout.xoff;
        self.pill_y = self.layout.top;
        self.pill_dirty = true;
        self.static_key = 0;
    }

    pub fn set_edit_mode(&mut self, on: bool) {
        self.edit_mode = on;
        self.dragging = false;
        self.drag = None;
        self.edit_sel = -1;
        if on {
            self.target_w = self.measure_target();
            self.cur_w = self.target_w;
            self.cur_h = self.pill_h;
            self.pill_x = (self.screen_w - self.cur_w) / 2 + self.layout.xoff;
            self.pill_y = self.layout.top;
            self.spring.value = 1.0;
            self.spring.animating = false;
            self.shown = true;
        } else {
            self.save_config();
        }
        self.pill_dirty = true;
        self.static_key = 0;
        self.edit_pm_key = u64::MAX;
    }

    // ------------------------------------------------------------- persistence

    /// Writes the same keys as the Linux app, to the platform config path.
    pub fn save_config(&self) {
        let path = config::config_path();
        if let Err(e) = config::save_layout(&path, &self.layout) {
            eprintln!("gabes-island: cannot save {}: {e}", path.display());
        }
    }

    // ------------------------------------------------------------------ tests

    /// Feeds a lyric payload without any network access (tests, previews,
    /// the headless benchmark).
    pub fn inject_lyrics(&mut self, text: &str) {
        let key = self.media.track_key();
        self.lyrics_req = key.clone();
        self.accept_lyrics(&key, text);
    }
}

fn hit(r: (f64, f64, f64, f64), x: f64, y: f64) -> bool {
    x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3
}

fn button_at(layout: &Layout, w: f64, h: f64, x: f64, y: f64) -> i32 {
    for (i, r) in geom::button_rects(layout, w, h).iter().enumerate() {
        if hit(*r, x, y) {
            return i as i32;
        }
    }
    -1
}

fn split_key(key: &str) -> (String, String) {
    match key.split_once('|') {
        Some((a, t)) => (a.to_string(), t.to_string()),
        None => (String::new(), key.to_string()),
    }
}

fn decode_cover(bytes: &[u8]) -> Option<Pixmap> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Pixmap::from_vec(img.into_raw(), tiny_skia::IntSize::from_wh(w, h)?)
}

fn layout_initial_width(l: &Layout) -> i32 {
    geom::pill_width(l, 120.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spectrum::mock::SilentSpectrum;
    use std::collections::HashMap;

    struct FakeOverlay {
        screen: (i32, i32),
        scale: f32,
        placed: HashMap<&'static str, (i32, i32, i32, i32)>,
        shown: HashMap<&'static str, bool>,
        presented: HashMap<&'static str, usize>,
    }

    impl FakeOverlay {
        fn new() -> FakeOverlay {
            FakeOverlay {
                screen: (1920, 1080),
                scale: 1.0,
                placed: HashMap::new(),
                shown: HashMap::new(),
                presented: HashMap::new(),
            }
        }
        fn is_shown(&self, s: Surface) -> bool {
            *self.shown.get(s.key()).unwrap_or(&false)
        }
    }

    impl Overlay for FakeOverlay {
        fn place(&mut self, s: Surface, x: i32, y: i32, w: i32, h: i32) {
            self.placed.insert(s.key(), (x, y, w, h));
        }
        fn present(&mut self, s: Surface, _pm: &Pixmap) {
            *self.presented.entry(s.key()).or_insert(0) += 1;
        }
        fn show(&mut self, s: Surface) {
            self.shown.insert(s.key(), true);
        }
        fn hide(&mut self, s: Surface) {
            self.shown.insert(s.key(), false);
        }
        fn screen(&self) -> (i32, i32) {
            self.screen
        }
        fn scale(&self) -> f32 {
            self.scale
        }
    }

    struct FakeMedia {
        m: Media,
        last: Option<PlayerAction>,
    }

    impl FakeMedia {
        fn new() -> FakeMedia {
            FakeMedia {
                m: Media {
                    playing: 1,
                    player: "com.spotify.client".into(),
                    app_name: "Spotify".into(),
                    title: "Get Lucky".into(),
                    artist: "Daft Punk".into(),
                    album: "RAM".into(),
                    art: String::new(),
                    cover: None,
                    length_us: 220_000_000,
                    pos_us: 0,
                },
                last: None,
            }
        }
    }

    impl MediaSource for FakeMedia {
        fn poll(&mut self) -> Option<Media> {
            Some(self.m.clone())
        }
        fn action(&mut self, a: PlayerAction) {
            self.last = Some(a);
        }
        fn backend(&self) -> &'static str {
            "fake"
        }
    }

    fn drive(app: &mut App, media: &mut FakeMedia, ov: &mut FakeOverlay, from: u128, to: u128) {
        let mut spec = SpectrumTracker::new(Box::new(SilentSpectrum));
        let mut now = from;
        while now < to {
            app.update(now, media, &mut spec, false);
            app.draw(now, ov);
            now += 16;
        }
    }

    fn test_layout() -> Layout {
        let mut l = Layout::default();
        l.anim = 0.3;
        l.wfall = 0;
        l.widgets = 0;
        l.anim = 0.3;
        l
    }

    #[test]
    fn pill_shows_while_playing_and_retracts_when_paused() {
        let mut app = App::new(test_layout(), 1.0, (1920, 1080), true);
        let mut media = FakeMedia::new();
        let mut ov = FakeOverlay::new();

        drive(&mut app, &mut media, &mut ov, 0, 1_000);
        assert!(ov.is_shown(Surface::Pill), "pill after 1s");
        let (x, y, w, h) = ov.placed[Surface::Pill.key()];
        assert_eq!(w, app.cur_w);
        assert!((x + w / 2 - 1920 / 2).abs() <= 1, "centred (x={x}, w={w})");
        assert_eq!(y, app.layout.top);
        assert!(h > 30 && w > 200, "size {w}x{h}");
        assert!(*ov.presented.get("pill").unwrap_or(&0) > 5);

        // pause: the spring retracts and the window is hidden
        media.m.playing = 2;
        drive(&mut app, &mut media, &mut ov, 1_000, 2_600);
        assert!(!ov.is_shown(Surface::Pill), "pill must hide when paused");
    }

    #[test]
    fn waterfall_spawns_words_from_synced_lyrics() {
        let mut l = test_layout();
        l.wfall = 1;
        l.wsync = 1;
        l.anim = 0.3;
        let mut app = App::new(l, 1.0, (1920, 1080), true);
        let mut media = FakeMedia::new();
        let mut ov = FakeOverlay::new();

        app.apply_media(0, media.m.clone());
        // words source while injecting avoids any network fetch
        app.layout.wsource = SOURCE_LYRICS;
        app.inject_lyrics("[00:01.00]alpha beta\n[00:03.00]gamma");
        assert!(app.lyrics.synced, "lrc should parse as synced");
        assert_eq!(app.lyrics.timed.len(), 3);

        media.m.pos_us = 1_100_000;
        let mut spec = SpectrumTracker::new(Box::new(SilentSpectrum));
        app.update(1_100, &mut media, &mut spec, false);
        app.draw(1_100, &mut ov);
        let texts: Vec<String> = app.fall_words().iter().map(|w| w.text.clone()).collect();
        assert!(texts.contains(&"alpha".to_string()), "{texts:?}");
        assert!(ov.is_shown(Surface::Waterfall));
        let rect = app.waterfall_rect();
        assert_eq!(rect.y, app.layout.top + app.pill_h);
        assert_eq!(rect.w, 560);
    }

    #[test]
    fn unsynced_lyrics_feed_the_waterfall_on_a_timer() {
        let mut l = test_layout();
        l.wfall = 1;
        l.wsync = 0;
        let mut app = App::new(l, 1.0, (1920, 1080), true);
        let mut media = FakeMedia::new();
        let mut ov = FakeOverlay::new();
        app.apply_media(0, media.m.clone());
        app.layout.wsource = SOURCE_LYRICS;
        app.inject_lyrics("plain words only here");
        assert!(!app.lyrics.synced);
        let mut spec = SpectrumTracker::new(Box::new(SilentSpectrum));
        app.update(200, &mut media, &mut spec, false);
        assert!(!app.fall_words().is_empty());
        app.draw(200, &mut ov);
        assert!(ov.is_shown(Surface::Waterfall));
    }

    #[test]
    fn edit_mode_selects_boxes_and_wheel_resizes() {
        let mut app = App::new(test_layout(), 1.0, (1920, 1080), true);
        let mut media = FakeMedia::new();
        app.apply_media(0, media.m.clone());
        app.set_edit_mode(true);
        assert!(app.edit_mode);
        app.cur_w = app.target_w;
        app.cur_h = app.pill_h;

        let boxes = geom::element_boxes(&app.layout, app.cur_w as f64, app.cur_h as f64);
        let b = boxes[3];
        let click = (b.0 + b.2 / 2.0, b.1 + b.3 / 2.0);
        app.on_button(
            Surface::Pill,
            1,
            true,
            Mods::default(),
            click,
            click,
            &mut media,
        );
        assert_eq!(app.edit_sel, 3, "spectrum box selected");

        let before = app.layout.spec;
        app.on_wheel(Surface::Pill, true, Mods::default());
        assert_eq!(app.layout.spec, before + 2.0);

        let before_h = app.layout.spec_h;
        app.on_wheel(
            Surface::Pill,
            false,
            Mods {
                alt: true,
                ..Mods::default()
            },
        );
        assert_eq!(app.layout.spec_h, before_h - 2.0);

        let before_anim = app.layout.anim;
        app.on_wheel(
            Surface::Pill,
            true,
            Mods {
                ctrl: true,
                ..Mods::default()
            },
        );
        assert!((app.layout.anim - before_anim - 0.1).abs() < 1e-9);
    }

    #[test]
    fn dragging_moves_the_bar_and_persists() {
        let dir = std::env::temp_dir().join(format!("island-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("GABES_CONFIG_DIR", &dir);

        let mut app = App::new(test_layout(), 1.0, (1920, 1080), true);
        let mut media = FakeMedia::new();
        app.apply_media(0, media.m.clone());
        app.set_edit_mode(true);
        app.cur_w = app.target_w;
        app.cur_h = app.pill_h;
        let (w, h) = (app.cur_w as f64, app.cur_h as f64);

        // a point between the text and time boxes
        let boxes = geom::element_boxes(&app.layout, w, h);
        let gap_x = boxes[1].0 + boxes[1].2 + 4.0;
        let gap_y = h / 2.0;
        app.on_button(
            Surface::Pill,
            1,
            true,
            Mods::default(),
            (gap_x, gap_y),
            (500.0, 20.0),
            &mut media,
        );
        assert!(app.dragging, "empty space starts a drag");
        app.on_cursor_move(Surface::Pill, gap_x, gap_y, (540.0, 45.0));
        assert_eq!(app.layout.xoff, 40);
        assert_eq!(app.layout.top, 35);
        app.on_button(
            Surface::Pill,
            1,
            false,
            Mods::default(),
            (gap_x, gap_y),
            (540.0, 45.0),
            &mut media,
        );
        assert!(!app.dragging);
        let saved = std::fs::read_to_string(config::config_path()).unwrap();
        assert!(saved.contains("x_offset=40"));
        assert!(saved.contains("top=35"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transport_buttons_forward_actions() {
        let mut app = App::new(test_layout(), 1.0, (1920, 1080), true);
        let mut media = FakeMedia::new();
        app.apply_media(0, media.m.clone());
        app.cur_w = 420;
        app.cur_h = 43;
        let rects = geom::button_rects(&app.layout, 420.0, 43.0);
        for (i, expected) in [
            (0usize, PlayerAction::Previous),
            (1, PlayerAction::PlayPause),
            (2, PlayerAction::Next),
        ] {
            let r = rects[i];
            app.on_button(
                Surface::Pill,
                1,
                true,
                Mods::default(),
                (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0),
                (0.0, 0.0),
                &mut media,
            );
            assert_eq!(media.last, Some(expected));
        }
        // the fourth button toggles edit mode
        let r = rects[3];
        app.on_button(
            Surface::Pill,
            1,
            true,
            Mods::default(),
            (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0),
            (0.0, 0.0),
            &mut media,
        );
        assert!(app.edit_mode);
    }

    #[test]
    fn leaving_edit_mode_while_paused_retracts_the_pill() {
        let mut app = App::new(test_layout(), 1.0, (1920, 1080), true);
        let mut media = FakeMedia::new();
        media.m.playing = 2;
        let mut ov = FakeOverlay::new();
        let mut spec = SpectrumTracker::new(Box::new(SilentSpectrum));

        app.apply_media(0, media.m.clone());
        app.set_edit_mode(true);
        app.update(16, &mut media, &mut spec, false);
        app.draw(16, &mut ov);
        assert!(ov.is_shown(Surface::Pill), "edit mode shows the pill");

        app.set_edit_mode(false);
        let mut now = 32u128;
        for _ in 0..200 {
            app.update(now, &mut media, &mut spec, false);
            app.draw(now, &mut ov);
            now += 16;
        }
        assert!(!ov.is_shown(Surface::Pill), "must hide once edit mode ends");
        assert!(!app.shown());
    }

    #[test]
    fn target_width_reflects_the_title() {
        let mut app = App::new(test_layout(), 1.0, (1920, 1080), true);
        let mut media = FakeMedia::new();
        media.m.title = "x".repeat(120);
        media.m.artist = "a".into();
        app.apply_media(0, media.m.clone());
        let wide = app.measure_target();
        media.m.title = "Hi".into();
        app.apply_media(10, media.m.clone());
        let narrow = app.measure_target();
        assert!(wide > narrow, "wide={wide} narrow={narrow}");
        assert!((island_core::config::PILL_MINW..=island_core::config::PILL_MAXW).contains(&wide));
    }
}
