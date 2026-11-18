//! Gabes Dynamic Island - a lightweight now-playing overlay for Cinnamon/X11.
//!
//! A small black pill at the top of the screen showing album art, title,
//! artist, elapsed time, an equalizer and a progress line for whatever plays
//! through MPRIS (Firefox, Spotify, mpv, ...). Hidden when nothing plays.
//!
//! Left click = play/pause, right click = next track.

use std::collections::HashMap;
use std::ffi::CString;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::raw::{c_int, c_uchar, c_ulong, c_void};
use std::process::{Child, Command, Stdio};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cairo::{Context, FontSlant, FontWeight, Format, ImageSurface, Operator, Surface};
use x11::xlib;

#[link(name = "Xext")]
extern "C" {
    fn XShapeCombineRectangles(
        dpy: *mut xlib::Display,
        dest: xlib::Window,
        dest_kind: c_int,
        x_off: c_int,
        y_off: c_int,
        rects: *mut c_void,
        n_rects: c_int,
        op: c_int,
        ordering: c_int,
    );
}
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedValue, Value};

const MPRIS_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";

const PILL_MINW: i32 = 240;
const PILL_MAXW: i32 = 1000;
const PAD: f64 = 8.0;
const DEFAULT_ART_W: f64 = 32.0;
const DEFAULT_TIME_W: f64 = 30.0;
const DEFAULT_SPEC_W: f64 = 26.0;
const DEFAULT_BTN_W: f64 = 18.0;
const DEFAULT_BTN_GAP: f64 = 6.0;
const DEFAULT_EXTRA_W: f64 = 60.0;
const MAX_BARS: usize = 32;

fn band_hz(i: usize) -> f32 {
    let t = i as f32 / (MAX_BARS - 1) as f32;
    60.0 * (12000.0f32 / 60.0).powf(t)
}
const DEFAULT_ACCENT: [f32; 3] = [0.48, 0.64, 0.97];
const WIDGET_FONTS: [&str; 2] = ["Noto Sans", "DejaVu Sans"];
const WIDGET_COLORS: [[f64; 3]; 6] = [
    [1.0, 1.0, 1.0],
    [0.48, 0.64, 0.97],
    [1.0, 0.45, 0.75],
    [0.40, 1.0, 0.80],
    [1.0, 0.90, 0.40],
    [1.0, 0.58, 0.30],
];

#[derive(Clone, Default)]
struct Media {
    playing: u8, // 1 playing, 2 paused, 0 stopped
    player: String,
    title: String,
    artist: String,
    art: String,
    length_us: i64,
    pos_us: i64,
}

struct FallWord {
    text: String,
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    angle: f64,
    spin: f64,
    font: usize,
    size: f64,
    color: [f64; 3],
    bold: bool,
    italic: bool,
    depth: i32,
    born: u128,
    life: u128,
}

// ---------------------------------------------------------------- helpers

fn unbox<'a>(v: &'a Value<'a>) -> &'a Value<'a> {
    let mut cur = v;
    while let Value::Value(inner) = cur {
        cur = inner;
    }
    cur
}

fn vstr(v: &Value) -> Option<String> {
    match unbox(v) {
        Value::Str(s) => Some(s.to_string()),
        _ => None,
    }
}

fn vi64(v: &Value) -> Option<i64> {
    match unbox(v) {
        Value::I64(n) => Some(*n),
        Value::U64(n) => Some(*n as i64),
        Value::I32(n) => Some(*n as i64),
        Value::U32(n) => Some(*n as i64),
        _ => None,
    }
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------- audio

/// Goertzel power at one frequency; cheap enough for a handful of bands.
fn goertzel(x: &[f32], rate: f32, freq: f32) -> f32 {
    let n = x.len();
    if n == 0 {
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

fn default_monitor() -> Option<String> {
    if let Ok(out) = Command::new("pactl").arg("get-default-sink").output() {
        let sink = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !sink.is_empty() {
            return Some(format!("{sink}.monitor"));
        }
    }
    if let Ok(out) = Command::new("pactl")
        .args(["list", "short", "sources"])
        .output()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(name) = line.split('\t').nth(1) {
                if name.ends_with(".monitor") {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

/// Captures the default sink monitor and publishes normalized band levels.
fn spawn_audio(active: Arc<AtomicBool>) -> Arc<Mutex<[f32; MAX_BARS]>> {
    let shared = Arc::new(Mutex::new([0.0f32; MAX_BARS]));
    let out = shared.clone();

    thread::spawn(move || loop {
        if !active.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(250));
            continue;
        }
        let monitor = match default_monitor() {
            Some(m) => m,
            None => {
                thread::sleep(Duration::from_secs(3));
                continue;
            }
        };
        let child = Command::new("parec")
            .args([
                "--format=s16le",
                "--rate=44100",
                "--channels=1",
                "-d",
                &monitor,
                "--latency-msec=20",
                "--raw",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();

        let mut child = match child {
            Ok(c) => c,
            Err(_) => {
                thread::sleep(Duration::from_secs(3));
                continue;
            }
        };
        let mut stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        };

        let mut peak = [0.01f32; MAX_BARS];
        let mut raw = [0u8; 2048];
        while stdout.read_exact(&mut raw).is_ok() {
            if !active.load(Ordering::Relaxed) {
                break;
            }
            let mut samples: Vec<f32> = raw
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                .collect();
            let n = samples.len();
            for (i, s) in samples.iter_mut().enumerate() {
                let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (n - 1) as f32).cos();
                *s *= w;
            }
            let mut levels = [0.0f32; MAX_BARS];
            for b in 0..MAX_BARS {
                let p = goertzel(&samples, 44100.0, band_hz(b));
                peak[b] = (peak[b] * 0.995).max(p).max(0.0005);
                let v = (p / peak[b]).powf(0.65).clamp(0.0, 1.0);
                levels[b] = v;
            }
            if let Ok(mut g) = out.lock() {
                for b in 0..MAX_BARS {
                    g[b] = g[b] * 0.45 + levels[b] * 0.55;
                }
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        thread::sleep(Duration::from_millis(800));
    });

    shared
}

// ---------------------------------------------------------------- MPRIS

fn list_names(conn: &Connection) -> Vec<String> {
    let reply = conn.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus"),
        "ListNames",
        &(),
    );
    match reply {
        Ok(msg) => msg.body().deserialize::<Vec<String>>().unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn get_all(conn: &Connection, dest: &str) -> Option<HashMap<String, OwnedValue>> {
    let reply = conn
        .call_method(
            Some(dest),
            MPRIS_PATH,
            Some(PROPS_IFACE),
            "GetAll",
            &(MPRIS_IFACE,),
        )
        .ok()?;
    reply.body().deserialize().ok()
}

fn get_i64(conn: &Connection, dest: &str, prop: &str) -> Option<i64> {
    let reply = conn
        .call_method(
            Some(dest),
            MPRIS_PATH,
            Some(PROPS_IFACE),
            "Get",
            &(MPRIS_IFACE, prop),
        )
        .ok()?;
    let v: OwnedValue = reply.body().deserialize().ok()?;
    vi64(&v)
}

fn parse_metadata<'a>(v: &'a Value<'a>, m: &mut Media) {
    if let Value::Dict(d) = unbox(v) {
        for (k, val) in d.iter() {
            let key = match unbox(k) {
                Value::Str(s) => s.to_string(),
                _ => continue,
            };
            match key.as_str() {
                "xesam:title" => {
                    if let Some(s) = vstr(val) {
                        m.title = s;
                    }
                }
                "xesam:artist" => {
                    if let Value::Array(a) = unbox(val) {
                        let names: Vec<String> =
                            a.iter().filter_map(|x| vstr(x)).collect();
                        m.artist = names.join(", ");
                    }
                }
                "mpris:artUrl" => {
                    if let Some(s) = vstr(val) {
                        m.art = s;
                    }
                }
                "mpris:length" => {
                    if let Some(n) = vi64(val) {
                        m.length_us = n;
                    }
                }
                _ => {}
            }
        }
    }
}

fn poll_media(conn: &Connection) -> Option<Media> {
    let names = list_names(conn);
    let mut paused: Option<Media> = None;

    for name in names
        .iter()
        .filter(|n| n.starts_with("org.mpris.MediaPlayer2."))
    {
        let props = match get_all(conn, name) {
            Some(p) => p,
            None => continue,
        };
        let mut m = Media::default();
        m.playing = match props.get("PlaybackStatus").map(|v| unbox(v)) {
            Some(Value::Str(s)) => match s.as_str() {
                "Playing" => 1,
                "Paused" => 2,
                _ => 0,
            },
            _ => 0,
        };
        if m.playing == 0 {
            continue;
        }
        if let Some(md) = props.get("Metadata") {
            parse_metadata(md, &mut m);
        }
        m.player = name.clone();
        if m.playing == 1 {
            m.pos_us = get_i64(conn, name, "Position").unwrap_or(0);
            return Some(m);
        }
        if paused.is_none() {
            m.pos_us = get_i64(conn, name, "Position").unwrap_or(0);
            paused = Some(m);
        }
    }
    paused
}

fn mpris_action(conn: &Connection, player: &str, method: &str) {
    let _ = conn.call_method(
        Some(player),
        MPRIS_PATH,
        Some(MPRIS_IFACE),
        method,
        &(),
    );
}

// ---------------------------------------------------------------- art

/// Loads a PNG/JPEG into a cairo surface with premultiplied alpha.
fn load_art_file(path: &str) -> Option<ImageSurface> {
    let bytes = std::fs::read(path).ok()?;
    let img = image::load_from_memory(&bytes).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    // downscale once with a good filter; cairo then only does a small 2x step
    let (img, w, h) = if w > 96 || h > 96 {
        let scale = 96.0 / (w.max(h) as f64);
        let nw = ((w as f64 * scale).round() as u32).max(1);
        let nh = ((h as f64 * scale).round() as u32).max(1);
        (
            image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3),
            nw,
            nh,
        )
    } else {
        (img, w, h)
    };
    let mut surf = ImageSurface::create(Format::ARgb32, w as i32, h as i32).ok()?;
    {
        let stride = surf.stride() as usize;
        let mut data = surf.data().ok()?;
        for (x, y, p) in img.enumerate_pixels() {
            let a = p[3] as u32;
            let idx = y as usize * stride + x as usize * 4;
            data[idx] = (p[2] as u32 * a / 255) as u8;
            data[idx + 1] = (p[1] as u32 * a / 255) as u8;
            data[idx + 2] = (p[0] as u32 * a / 255) as u8;
            data[idx + 3] = a as u8;
        }
    }
    surf.mark_dirty();
    Some(surf)
}

fn file_path_from_url(url: &str) -> Option<String> {
    url.strip_prefix("file://").map(urldecode)
}

fn cache_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var("XDG_CACHE_HOME")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| std::path::PathBuf::from(h).join(".cache"))
        })?;
    let dir = base.join("gabes-island");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Fetches remote cover art (Spotify, browsers) with curl into a local cache.
fn download_art(url: &str) -> Option<String> {
    let dir = cache_dir()?;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in url.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    let path = dir.join(format!("art-{hash:016x}.img"));
    let p = path.to_string_lossy().to_string();
    if let Ok(m) = std::fs::metadata(&p) {
        if m.len() > 0 {
            return Some(p);
        }
    }
    let ok = Command::new("curl")
        .args(["-fsSL", "--max-time", "4", "-o", p.as_str(), url])
        .status()
        .map(|st| st.success())
        .unwrap_or(false);
    if ok && std::fs::metadata(&p).map(|m| m.len() > 0).unwrap_or(false) {
        Some(p)
    } else {
        None
    }
}

fn get_string_prop(conn: &Connection, dest: &str, iface: &str, prop: &str) -> Option<String> {
    let reply = conn
        .call_method(
            Some(dest),
            MPRIS_PATH,
            Some(PROPS_IFACE),
            "Get",
            &(iface, prop),
        )
        .ok()?;
    let v: OwnedValue = reply.body().deserialize().ok()?;
    vstr(&v)
}

/// Finds an application icon in the common XDG icon theme layouts.
fn find_icon_png(name: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let bases = [
        "/usr/share/icons".to_string(),
        format!("{home}/.local/share/icons"),
        format!("{home}/.icons"),
    ];
    let themes = [
        "hicolor", "Adwaita", "Papirus", "Papirus-Dark", "Mint-Y", "Mint-Y-Dark",
        "Mint-X", "Yaru", "gnome", "DMZ-White", "DMZ-Black",
    ];
    let sizes = ["512x512", "256x256", "128x128", "96x96", "64x64", "48x48", "32x32"];
    let subs = ["apps", "devices", "mimetypes", "places"];

    for base in &bases {
        for theme in themes {
            for size in sizes {
                for sub in subs {
                    let p = format!("{base}/{theme}/{size}/{sub}/{name}.png");
                    if std::path::Path::new(&p).is_file() {
                        return Some(p);
                    }
                }
            }
            for size in ["48", "64", "32", "96", "128", "22", "24"] {
                let p = format!("{base}/{theme}/apps/{size}/{name}.png");
                if std::path::Path::new(&p).is_file() {
                    return Some(p);
                }
            }
        }
    }
    let p = format!("/usr/share/pixmaps/{name}.png");
    if std::path::Path::new(&p).is_file() {
        return Some(p);
    }
    None
}

/// Resolves the icon of the application that owns the MPRIS player.
fn load_player_icon(conn: &Connection, player: &str) -> Option<ImageSurface> {
    let mut names: Vec<String> = Vec::new();
    if let Some(seg) = player.strip_prefix("org.mpris.MediaPlayer2.") {
        if let Some(first) = seg.split('.').next() {
            names.push(first.to_string());
        }
    }
    if let Some(de) = get_string_prop(conn, player, "org.mpris.MediaPlayer2", "DesktopEntry") {
        let base = de.trim_end_matches(".desktop").to_string();
        if let Some(last) = base.split('.').last() {
            names.push(last.to_string());
        }
        names.push(base);
    }
    names.push("multimedia-player".to_string());

    for name in names {
        if let Some(path) = find_icon_png(&name) {
            if let Some(surf) = load_art_file(&path) {
                return Some(surf);
            }
        }
    }
    None
}

// ---------------------------------------------------------------- drawing

fn rounded_rect(cr: &Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(h / 2.0).min(w / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
    cr.arc(x + r, y + r, r, std::f64::consts::PI, 3.0 * std::f64::consts::FRAC_PI_2);
    cr.close_path();
}

fn set_font_ex(cr: &Context, family: &str, size: f64, bold: bool, italic: bool) {
    cr.select_font_face(
        family,
        if italic { FontSlant::Italic } else { FontSlant::Normal },
        if bold { FontWeight::Bold } else { FontWeight::Normal },
    );
    cr.set_font_size(size);
}

fn set_font(cr: &Context, size: f64, bold: bool) {
    set_font_ex(cr, "Noto Sans", size, bold, false);
}

const CJK_JP: &str = "Noto Sans CJK JP";
const CJK_KR: &str = "Noto Sans CJK KR";

fn has_hangul(text: &str) -> bool {
    text.chars().any(|c| {
        let u = c as u32;
        (0xAC00..=0xD7A3).contains(&u) || (0x1100..=0x11FF).contains(&u)
    })
}

fn has_cjk(text: &str) -> bool {
    text.chars().any(|c| {
        let u = c as u32;
        (0x3000..=0x303F).contains(&u)
            || (0x3040..=0x30FF).contains(&u)
            || (0x3400..=0x4DBF).contains(&u)
            || (0x4E00..=0x9FFF).contains(&u)
            || (0xF900..=0xFAFF).contains(&u)
            || (0xFF66..=0xFF9D).contains(&u)
    })
}

/// Picks a font that actually has glyphs for this text (no tofu boxes).
fn font_for_text<'a>(text: &str, fallback: &'a str) -> &'a str {
    if has_hangul(text) {
        CJK_KR
    } else if has_cjk(text) {
        CJK_JP
    } else {
        fallback
    }
}

fn text_width(cr: &Context, text: &str) -> f64 {
    cr.text_extents(text).map(|e| e.x_advance()).unwrap_or(0.0)
}

fn ellipsize(cr: &Context, text: &str, maxw: f64) -> String {
    if text.is_empty() || text_width(cr, text) <= maxw {
        return text.to_string();
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let cut = if mid < chars.len() { chars[mid].0 } else { text.len() };
        let candidate = format!("{}\u{2026}", &text[..cut]);
        if text_width(cr, &candidate) <= maxw {
            lo = mid;
        } else {
            hi = mid.saturating_sub(1);
        }
    }
    let cut = if lo < chars.len() { chars[lo].0 } else { text.len() };
    format!("{}\u{2026}", &text[..cut])
}

fn fmt_time(us: i64) -> String {
    let secs = (us.max(0) / 1_000_000) as i64;
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn config_path() -> std::path::PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| std::path::PathBuf::from(h).join(".config"))
        })
        .unwrap_or_default();
    base.join("gabes").join("island.conf")
}

#[derive(Clone)]
struct Layout {
    top: i32,
    height: i32,
    xoff: i32,
    extra: f64,
    art: f64,
    time: f64,
    spec: f64,
    btn: f64,
    gap: f64,
    anim: f64,
    padx: f64,
    art_h: f64,
    text_h: f64,
    time_h: f64,
    spec_h: f64,
    btn_h: f64,
    widgets: i32,
    wmin: u64,
    wmax: u64,
    wsource: i32,
    wplace: i32,
    w3d: i32,
    wcont: i32,
    wms: u64,
    wsync: i32,
    wfall: i32,
    wtrail: i32,
    wbpm: u64,
    wanti: i32,
    wdrift: u64,
    asr: i32,
    warmup: u64,
    asr_python: String,
    asr_script: String,
    asr_model: String,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            top: 10,
            height: 43,
            xoff: 0,
            extra: DEFAULT_EXTRA_W,
            art: DEFAULT_ART_W,
            time: DEFAULT_TIME_W,
            spec: DEFAULT_SPEC_W,
            btn: DEFAULT_BTN_W,
            gap: DEFAULT_BTN_GAP,
            anim: 1.5,
            padx: 0.0,
            art_h: 32.0,
            text_h: 26.0,
            time_h: 14.0,
            spec_h: 28.0,
            btn_h: 18.0,
            widgets: 1,
            wmin: 20,
            wmax: 45,
            wsource: 1,
            wplace: 2,
            w3d: 1,
            wcont: 1,
            wms: 500,
            wsync: 1,
            wfall: 1,
            wtrail: 1,
            wbpm: 0,
            wanti: 1,
            wdrift: 120,
            asr: 1,
            warmup: 10_000,
            asr_python: "~/.local/share/gabes-island/venv/bin/python".into(),
            asr_script: "~/.local/share/gabes-island/asr_vosk.py".into(),
            asr_model: "~/.local/share/gabes-island/vosk-model-small-en-us-0.15".into(),
        }
    }
}

fn load_words() -> Vec<String> {
    let path = config_path().with_file_name("words.txt");
    let mut words: Vec<String> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                words.push(line.to_string());
            }
        }
    }
    if words.is_empty() {
        words = [
            "hej",
            "spokojnie",
            "kawa?",
            "kod się kompiluje",
            "GTA 6?",
            "random",
            "nice",
            "idzie dobrze",
            "prawie działa",
            "dzień dobry",
            "nie panikuj",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
    }
    words
}

fn load_layout() -> Layout {
    let mut l = Layout::default();
    if let Ok(text) = std::fs::read_to_string(config_path()) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim();
                match k.trim() {
                    "top" => l.top = v.parse().unwrap_or(l.top),
                    "height" => l.height = v.parse().unwrap_or(l.height),
                    "x_offset" => l.xoff = v.parse().unwrap_or(l.xoff),
                    "extra_width" => l.extra = v.parse().unwrap_or(l.extra),
                    "art_w" => l.art = v.parse().unwrap_or(l.art),
                    "time_w" => l.time = v.parse().unwrap_or(l.time),
                    "spec_w" => l.spec = v.parse().unwrap_or(l.spec),
                    "btn_w" => l.btn = v.parse().unwrap_or(l.btn),
                    "btn_gap" => l.gap = v.parse().unwrap_or(l.gap),
                    "anim_secs" => l.anim = v.parse().unwrap_or(l.anim),
                    "pad_x" => l.padx = v.parse().unwrap_or(l.padx),
                    "art_h" => l.art_h = v.parse().unwrap_or(l.art_h),
                    "text_h" => l.text_h = v.parse().unwrap_or(l.text_h),
                    "time_h" => l.time_h = v.parse().unwrap_or(l.time_h),
                    "spec_h" => l.spec_h = v.parse().unwrap_or(l.spec_h),
                    "btn_h" => l.btn_h = v.parse().unwrap_or(l.btn_h),
                    "widgets" => l.widgets = v.parse().unwrap_or(l.widgets),
                    "widget_min" => l.wmin = v.parse().unwrap_or(l.wmin),
                    "widget_max" => l.wmax = v.parse().unwrap_or(l.wmax),
                    "widget_source" => {
                        l.wsource = match v {
                            "lyrics" => 1,
                            "words" => 0,
                            _ => v.parse().unwrap_or(l.wsource),
                        }
                    }
                    "widget_place" => {
                        l.wplace = match v {
                            "random" => 0,
                            "island" => 1,
                            "both" => 2,
                            _ => v.parse().unwrap_or(l.wplace),
                        }
                    }
                    "widget_3d" => l.w3d = v.parse().unwrap_or(l.w3d),
                    "widget_continuous" => l.wcont = v.parse().unwrap_or(l.wcont),
                    "widget_word_ms" => l.wms = v.parse().unwrap_or(l.wms),
                    "widget_sync" => l.wsync = v.parse().unwrap_or(l.wsync),
                    "widget_waterfall" => l.wfall = v.parse().unwrap_or(l.wfall),
                    "widget_trails" => l.wtrail = v.parse().unwrap_or(l.wtrail),
                    "widget_bpm" => l.wbpm = v.parse().unwrap_or(l.wbpm),
                    "widget_antidesync" => l.wanti = v.parse().unwrap_or(l.wanti),
                    "widget_max_drift_ms" => l.wdrift = v.parse().unwrap_or(l.wdrift),
                    "asr_enabled" => l.asr = v.parse().unwrap_or(l.asr),
                    "widget_warmup_ms" => l.warmup = v.parse().unwrap_or(l.warmup),
                    "asr_python" => l.asr_python = v.to_string(),
                    "asr_script" => l.asr_script = v.to_string(),
                    "asr_model" => l.asr_model = v.to_string(),
                    _ => {}
                }
            }
        }
    }
    l
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn parse_json_string(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\":\"", key);
    let start = body.find(&needle)? + needle.len();
    let mut out = String::new();
    let mut chars = body[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(cp) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(cp) {
                            out.push(ch);
                        }
                    }
                }
                other => out.push(other),
            },
            _ => out.push(c),
        }
    }
    if out.trim().is_empty() {
        None
    } else {
        Some(out)
    }
}

fn clean_track_title(title: &str) -> String {
    let mut t = title.to_string();
    for sep in [" (", " [", " - ", " \u{2013} "] {
        if let Some(i) = t.find(sep) {
            t.truncate(i);
        }
    }
    t.trim().to_string()
}

fn parse_lrc_time(tag: &str) -> Option<i64> {
    let mut parts = tag.split(':');
    let minutes: i64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    Some(((minutes * 60) as f64 * 1000.0 + seconds * 1000.0) as i64)
}

fn clean_word(w: &str) -> String {
    w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
        .to_string()
}

/// Turns LRC into a flat list of (milliseconds, word), one entry per word.
fn parse_lrc(text: &str) -> Vec<(i64, String)> {
    let mut lines: Vec<(i64, String)> = Vec::new();
    for line in text.lines() {
        let mut rest = line.trim();
        let mut times: Vec<i64> = Vec::new();
        while rest.starts_with('[') {
            if let Some(end) = rest.find(']') {
                if let Some(t) = parse_lrc_time(&rest[1..end]) {
                    times.push(t);
                }
                rest = rest[end + 1..].trim_start();
            } else {
                break;
            }
        }
        if times.is_empty() || rest.is_empty() {
            continue;
        }
        for t in times {
            lines.push((t, rest.to_string()));
        }
    }
    lines.sort_by_key(|l| l.0);
    let mut words: Vec<(i64, String)> = Vec::new();
    for i in 0..lines.len() {
        let (start, ref text) = lines[i];
        let next = lines.get(i + 1).map(|l| l.0).unwrap_or(start + 5000);
        let ws: Vec<String> = text
            .split_whitespace()
            .map(clean_word)
            .filter(|w| !w.is_empty())
            .collect();
        if ws.is_empty() {
            continue;
        }
        let dur = (next - start).max(200);
        let slot = dur / ws.len() as i64;
        for (j, w) in ws.iter().enumerate() {
            words.push((start + j as i64 * slot, w.clone()));
        }
    }
    words
}

fn strip_lrc(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let mut s = line;
        while s.starts_with('[') {
            if let Some(i) = s.find(']') {
                s = &s[i + 1..];
            } else {
                break;
            }
        }
        let s = s.trim();
        if !s.is_empty() {
            out.push(s.to_string());
        }
    }
    out.join(" ")
}

fn fetch_lyrics(artist: String, title: String, tx: mpsc::Sender<(String, String)>) {
    let key = format!("{artist}|{title}");
    let mut artists: Vec<String> = vec![artist.clone()];
    if let Some(first) = artist.split(',').next() {
        let first = first.trim().to_string();
        if first != artist {
            artists.push(first);
        }
    }
    let mut titles: Vec<String> = vec![title.clone()];
    let clean = clean_track_title(&title);
    if clean != title {
        titles.push(clean);
    }

    // Spotify does not expose lyrics publicly; use its metadata with LRCLIB.
    for a in &artists {
        for t in &titles {
            if a.is_empty() || t.is_empty() {
                continue;
            }
            let url = format!(
                "https://lrclib.net/api/get?artist_name={}&track_name={}",
                url_encode(a),
                url_encode(t)
            );
            if let Ok(out) = Command::new("curl")
                .args([
                    "-fsSL",
                    "--max-time",
                    "6",
                    "-H",
                    "User-Agent: gabes-island/0.1",
                    url.as_str(),
                ])
                .output()
            {
                if out.status.success() {
                    let body = String::from_utf8_lossy(&out.stdout);
                    if let Some(ly) = parse_json_string(&body, "syncedLyrics") {
                        let _ = tx.send((key.clone(), ly));
                        return;
                    }
                    if let Some(ly) = parse_json_string(&body, "plainLyrics") {
                        let _ = tx.send((key.clone(), ly));
                        return;
                    }
                }
            }
        }
    }

    // last resort
    let url = format!(
        "https://api.lyrics.ovh/v1/{}/{}",
        url_encode(&artist),
        url_encode(&title)
    );
    if let Ok(out) = Command::new("curl")
        .args(["-fsSL", "--max-time", "6", url.as_str()])
        .output()
    {
        if out.status.success() {
            let body = String::from_utf8_lossy(&out.stdout);
            if let Some(ly) = parse_json_string(&body, "lyrics") {
                let _ = tx.send((key, ly));
            }
        }
    }
}

fn draw_fall_text(
    cr: &Context,
    word: &FallWord,
    x: f64,
    y: f64,
    angle: f64,
    alpha: f64,
) {
    if alpha <= 0.001 {
        return;
    }
    let fam = font_for_text(
        &word.text,
        WIDGET_FONTS[word.font.min(WIDGET_FONTS.len() - 1)],
    );
    set_font_ex(cr, fam, word.size, word.bold, word.italic);
    let tw = text_width(cr, &word.text);
    let _ = cr.save();
    cr.translate(x, y);
    cr.rotate(angle);
    for d in (1..=word.depth).rev() {
        cr.set_source_rgba(
            word.color[0] * 0.35,
            word.color[1] * 0.35,
            word.color[2] * 0.45,
            0.55 * alpha,
        );
        cr.move_to(-tw / 2.0 + d as f64 * 0.8, word.size * 0.35 + d as f64 * 0.8);
        let _ = cr.show_text(&word.text);
    }
    cr.set_source_rgba(word.color[0], word.color[1], word.color[2], 0.96 * alpha);
    cr.move_to(-tw / 2.0, word.size * 0.35);
    let _ = cr.show_text(&word.text);
    let _ = cr.restore();
}

fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    path.to_string()
}

fn mmss(ms: i64) -> String {
    let s = (ms.max(0) / 1000) as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn parse_mmss(s: &str) -> Option<i64> {
    let (m, sec) = s.split_once(':')?;
    let m: i64 = m.parse().ok()?;
    let sec: i64 = sec.parse().ok()?;
    Some((m * 60 + sec) * 1000)
}

fn norm_word(w: &str) -> String {
    w.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn similar(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if a.len() >= 4 && b.len() >= 4 && (a.starts_with(b) || b.starts_with(a)) {
        return true;
    }
    a.len().max(b.len()) >= 3 && levenshtein(a, b) <= 1
}

fn spawn_asr(
    python: &str,
    script: &str,
    model: &str,
    tx: mpsc::Sender<String>,
    slot: Arc<Mutex<Option<Child>>>,
    active: Arc<AtomicBool>,
) {
    let python = expand_home(python);
    let script = expand_home(script);
    let model = expand_home(model);
    if !std::path::Path::new(&python).is_file()
        || !std::path::Path::new(&script).is_file()
        || !std::path::Path::new(&model).is_dir()
    {
        eprintln!("gabes-island: ASR disabled (missing python/script/model)");
        return;
    }
    thread::spawn(move || loop {
        if !active.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(300));
            continue;
        }
        let cmd = format!(
            "parec --format=s16le --rate=16000 --channels=1 -d \"$(pactl get-default-sink).monitor\" --raw 2>/dev/null | exec '{}' '{}' '{}'",
            python, script, model
        );
        let child = Command::new("sh")
            .args(["-c", cmd.as_str()])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(_) => {
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        let stdout = child.stdout.take();
        if let Ok(mut s) = slot.lock() {
            *s = Some(child);
        }
        if let Some(out) = stdout {
            for line in BufReader::new(out).lines().flatten() {
                if !active.load(Ordering::Relaxed) {
                    break;
                }
                let _ = tx.send(line);
            }
        }
        if let Ok(mut s) = slot.lock() {
            if let Some(mut c) = s.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
        thread::sleep(Duration::from_millis(400));
    });
}

// ---------------------------------------------------------------- X11 app

struct App {
    dpy: *mut xlib::Display,
    root: xlib::Window,
    win: xlib::Window,
    editwin: xlib::Window,
    editbuf: xlib::Pixmap,
    edit_hover: bool,
    vis: *mut xlib::Visual,
    sw: i32,
    sh: i32,
    pillh: i32,
    topmargin: i32,
    extra_width: f64,
    art_w: f64,
    time_w: f64,
    spec_w: f64,
    btn_w: f64,
    btn_gap: f64,
    anim_secs: f64,
    pad_x: f64,
    art_h: f64,
    text_h: f64,
    time_h: f64,
    spec_h: f64,
    btn_h: f64,
    widgets_enabled: bool,
    widget_min: u64,
    widget_max: u64,
    widgetwin: xlib::Window,
    widgetbuf: xlib::Pixmap,
    widget_bufw: i32,
    widget_bufh: i32,
    widget_w: i32,
    widget_h: i32,
    widget_x: i32,
    widget_y: i32,
    widget_active: bool,
    widget_start: u128,
    widget_next: u128,
    widget_duration: u128,
    widget_text: String,
    widget_font: usize,
    widget_font_size: f64,
    widget_color: [f64; 3],
    widget_italic: bool,
    widget_bold: bool,
    widget_depth: i32,
    widget_angle: f64,
    words: Vec<String>,
    lyrics_words: Vec<String>,
    lyrics_req: String,
    lyrics_tx: mpsc::Sender<(String, String)>,
    lyrics_rx: mpsc::Receiver<(String, String)>,
    widget_source: u8,
    widget_place: u8,
    widget_3d: bool,
    widget_continuous: bool,
    widget_word_ms: u128,
    widget_sync: bool,
    widget_display_ms: u128,
    plain_idx: usize,
    last_word: String,
    lyrics_timed: Vec<(i64, String)>,
    lyrics_synced: bool,
    last_word_idx: i64,
    waterfall: bool,
    trails: bool,
    fallwin: xlib::Window,
    fallbuf: xlib::Pixmap,
    fall_bufw: i32,
    fall_bufh: i32,
    fall_x0: i32,
    fall_y0: i32,
    fall_w: i32,
    fall_h: i32,
    fall_last_draw: u128,
    fall_mapped: bool,
    fall_words: Vec<FallWord>,
    fall_next: u128,
    fall_last_tick: u128,
    fall_last_idx: i64,
    widget_bpm: u64,
    schedule_cursor: usize,
    antidesync: bool,
    max_drift_ms: i64,
    sync_offset_ms: i64,
    drift_ema: f64,
    drift_n: u32,
    last_pos_ms: i64,
    lyrics_raw: Vec<(i64, String)>,
    recognized: Vec<(i64, String)>,
    asr_enabled: bool,
    asr_tx: mpsc::Sender<String>,
    asr_rx: mpsc::Receiver<String>,
    asr_child: Arc<Mutex<Option<Child>>>,
    asr_active: Arc<AtomicBool>,
    aligned: Vec<Option<i64>>,
    align_path: String,
    warmup_ms: i64,
    display_ready: bool,
    rng: u64,
    edit_sel: i32,
    x_offset: i32,
    edit_mode: bool,
    dragging: bool,
    drag_start: (i32, i32, i32, i32),
    hover_btn: i32,
    btn_rects: [(f64, f64, f64, f64); 4],
    curw: i32,
    curh: i32,
    anim: f64,
    anim_vel: f64,
    dir: i32,
    animating: bool,
    mapped: bool,
    media: Media,
    have_media: bool,
    targetw: i32,
    art: Option<ImageSurface>,
    loaded_art: String,
    icon: Option<ImageSurface>,
    loaded_icon: String,
    pos_base_us: i64,
    pos_base_ms: u128,
    player: String,
    net_active: xlib::Atom,
    net_wm_state: xlib::Atom,
    net_fullscreen: xlib::Atom,
    gc: xlib::GC,
    buf: xlib::Pixmap,
    bufw: i32,
    bufh: i32,
    sbuf: xlib::Pixmap,
    sbufw: i32,
    sbufh: i32,
    static_key: u64,

    bar_levels: [f32; MAX_BARS],
    bands: Arc<Mutex<[f32; MAX_BARS]>>,
    audio_active: Arc<AtomicBool>,
    art_tx: mpsc::Sender<(String, String)>,
    art_rx: mpsc::Receiver<(String, String)>,
}

impl App {
    unsafe fn new(
        layout: Layout,
        bands: Arc<Mutex<[f32; MAX_BARS]>>,
        audio_active: Arc<AtomicBool>,
        art_tx: mpsc::Sender<(String, String)>,
        art_rx: mpsc::Receiver<(String, String)>,
        lyrics_tx: mpsc::Sender<(String, String)>,
        lyrics_rx: mpsc::Receiver<(String, String)>,
        asr_tx: mpsc::Sender<String>,
        asr_rx: mpsc::Receiver<String>,
        asr_child: Arc<Mutex<Option<Child>>>,
        asr_active: Arc<AtomicBool>,
    ) -> Option<App> {
        let dpy = xlib::XOpenDisplay(ptr::null());
        if dpy.is_null() {
            return None;
        }
        let screen = xlib::XDefaultScreen(dpy);
        let root = xlib::XRootWindow(dpy, screen);
        let mut vi: xlib::XVisualInfo = std::mem::zeroed();
        if xlib::XMatchVisualInfo(dpy, screen, 32, xlib::TrueColor, &mut vi) == 0 {
            eprintln!("gabes-island: no 32-bit visual");
            return None;
        }
        let vis = vi.visual;
        let cmap = xlib::XCreateColormap(dpy, root, vis, xlib::AllocNone);
        let sw = xlib::XDisplayWidth(dpy, screen);
        let sh = xlib::XDisplayHeight(dpy, screen);
        let tallest = layout
            .art_h
            .max(layout.text_h)
            .max(layout.time_h)
            .max(layout.spec_h)
            .max(layout.btn_h);
        let pillh = layout
            .height
            .clamp(30, 120)
            .max(((tallest + 6.0) as i32).max(30));

        let mut wa: xlib::XSetWindowAttributes = std::mem::zeroed();
        wa.override_redirect = xlib::True;
        wa.colormap = cmap;
        wa.background_pixel = 0;
        wa.border_pixel = 0;
        wa.event_mask = xlib::ExposureMask | xlib::ButtonPressMask
            | xlib::ButtonReleaseMask | xlib::PointerMotionMask | xlib::LeaveWindowMask;
        let win = xlib::XCreateWindow(
            dpy,
            root,
            (sw - PILL_MINW) / 2,
            layout.top,
            PILL_MINW as u32,
            pillh as u32,
            0,
            32,
            xlib::InputOutput as u32,
            vis,
            (xlib::CWOverrideRedirect | xlib::CWColormap | xlib::CWBackPixel
                | xlib::CWBorderPixel | xlib::CWEventMask) as c_ulong,
            &mut wa,
        );
        let name = CString::new("gabes-island").unwrap();
        xlib::XStoreName(dpy, win, name.as_ptr());

        // always-visible mini edit button in the top-right corner
        let mut ewa: xlib::XSetWindowAttributes = std::mem::zeroed();
        ewa.override_redirect = xlib::True;
        ewa.colormap = cmap;
        ewa.background_pixel = 0;
        ewa.border_pixel = 0;
        ewa.event_mask = xlib::ExposureMask | xlib::ButtonPressMask
            | xlib::PointerMotionMask | xlib::LeaveWindowMask;
        let editwin = xlib::XCreateWindow(
            dpy,
            root,
            sw / 2 - 13,
            12,
            26,
            26,
            0,
            32,
            xlib::InputOutput as u32,
            vis,
            (xlib::CWOverrideRedirect | xlib::CWColormap | xlib::CWBackPixel
                | xlib::CWBorderPixel | xlib::CWEventMask) as c_ulong,
            &mut ewa,
        );
        let ename = CString::new("gabes-island-edit").unwrap();
        xlib::XStoreName(dpy, editwin, ename.as_ptr());
        xlib::XMapRaised(dpy, editwin);
        let editbuf = xlib::XCreatePixmap(dpy, editwin, 26, 26, 32);

        // random-text widget window: click-through, only mapped when shown
        let mut wwa: xlib::XSetWindowAttributes = std::mem::zeroed();
        wwa.override_redirect = xlib::True;
        wwa.colormap = cmap;
        wwa.background_pixel = 0;
        wwa.border_pixel = 0;
        wwa.event_mask = xlib::ExposureMask;
        let widgetwin = xlib::XCreateWindow(
            dpy,
            root,
            0,
            0,
            10,
            10,
            0,
            32,
            xlib::InputOutput as u32,
            vis,
            (xlib::CWOverrideRedirect | xlib::CWColormap | xlib::CWBackPixel
                | xlib::CWBorderPixel | xlib::CWEventMask) as c_ulong,
            &mut wwa,
        );
        let wname = CString::new("gabes-island-widget").unwrap();
        xlib::XStoreName(dpy, widgetwin, wname.as_ptr());
        // empty input shape: the widget never eats clicks
        XShapeCombineRectangles(
            dpy,
            widgetwin,
            2,
            0,
            0,
            ptr::null_mut(),
            0,
            0,
            0,
        );
        let widgetbuf = xlib::XCreatePixmap(dpy, widgetwin, 10, 10, 32);

        // fullscreen, click-through waterfall overlay
        let mut fwa: xlib::XSetWindowAttributes = std::mem::zeroed();
        fwa.override_redirect = xlib::True;
        fwa.colormap = cmap;
        fwa.background_pixel = 0;
        fwa.border_pixel = 0;
        fwa.event_mask = xlib::ExposureMask;
        let fallwin = xlib::XCreateWindow(
            dpy,
            root,
            0,
            0,
            sw as u32,
            sh as u32,
            0,
            32,
            xlib::InputOutput as u32,
            vis,
            (xlib::CWOverrideRedirect | xlib::CWColormap | xlib::CWBackPixel
                | xlib::CWBorderPixel | xlib::CWEventMask) as c_ulong,
            &mut fwa,
        );
        let fname = CString::new("gabes-island-fall").unwrap();
        xlib::XStoreName(dpy, fallwin, fname.as_ptr());
        XShapeCombineRectangles(
            dpy,
            fallwin,
            2,
            0,
            0,
            ptr::null_mut(),
            0,
            0,
            0,
        );
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x1234_5678_9abc_def1)
            | 1;
        xlib::XSelectInput(dpy, root, xlib::SubstructureNotifyMask | xlib::StructureNotifyMask);

        let atom = |n: &str| -> xlib::Atom {
            let c = CString::new(n).unwrap();
            xlib::XInternAtom(dpy, c.as_ptr(), xlib::False)
        };

        Some(App {
            dpy,
            root,
            win,
            editwin,
            editbuf,
            edit_hover: false,
            vis,
            sw,
            sh,
            pillh,
            topmargin: layout.top,
            extra_width: layout.extra,
            art_w: layout.art,
            time_w: layout.time,
            spec_w: layout.spec,
            btn_w: layout.btn,
            btn_gap: layout.gap,
            anim_secs: layout.anim,
            pad_x: layout.padx,
            art_h: layout.art_h,
            text_h: layout.text_h,
            time_h: layout.time_h,
            spec_h: layout.spec_h,
            btn_h: layout.btn_h,
            widgets_enabled: layout.widgets != 0,
            widget_min: layout.wmin.max(3),
            widget_max: layout.wmax.max(layout.wmin.max(3) + 1),
            widgetwin,
            widgetbuf,
            widget_bufw: 10,
            widget_bufh: 10,
            widget_w: 10,
            widget_h: 10,
            widget_x: 0,
            widget_y: 0,
            widget_active: false,
            widget_start: 0,
            widget_next: now_ms() + 8000,
            widget_duration: 3200,
            widget_text: String::new(),
            widget_font: 0,
            widget_font_size: 16.0,
            widget_color: [1.0, 1.0, 1.0],
            widget_italic: false,
            widget_bold: true,
            widget_depth: 3,
            widget_angle: 0.0,
            words: load_words(),
            lyrics_words: Vec::new(),
            lyrics_req: String::new(),
            lyrics_tx,
            lyrics_rx,
            widget_source: layout.wsource as u8,
            widget_place: layout.wplace as u8,
            widget_3d: layout.w3d != 0,
            widget_continuous: layout.wcont != 0,
            widget_word_ms: layout.wms.clamp(120, 2000) as u128,
            widget_sync: layout.wsync != 0,
            widget_display_ms: 500,
            plain_idx: 0,
            last_word: String::new(),
            lyrics_timed: Vec::new(),
            lyrics_synced: false,
            last_word_idx: -1,
            waterfall: layout.wfall != 0,
            trails: layout.wtrail != 0,
            fallwin,
            fallbuf: 0,
            fall_bufw: 0,
            fall_bufh: 0,
            fall_x0: 0,
            fall_y0: 0,
            fall_w: 0,
            fall_h: 0,
            fall_last_draw: 0,
            fall_mapped: false,
            fall_words: Vec::new(),
            fall_next: 0,
            fall_last_tick: 0,
            fall_last_idx: -1,
            widget_bpm: layout.wbpm,
            schedule_cursor: 0,
            antidesync: layout.wanti != 0,
            max_drift_ms: layout.wdrift as i64,
            sync_offset_ms: 0,
            drift_ema: 0.0,
            drift_n: 0,
            last_pos_ms: 0,
            lyrics_raw: Vec::new(),
            recognized: Vec::new(),
            asr_enabled: layout.asr != 0,
            asr_tx: asr_tx.clone(),
            asr_rx,
            asr_child: asr_child.clone(),
            asr_active: asr_active.clone(),
            aligned: Vec::new(),
            align_path: String::new(),
            warmup_ms: layout.warmup as i64,
            display_ready: layout.warmup == 0,
            rng: seed,
            edit_sel: -1,
            x_offset: layout.xoff,
            edit_mode: false,
            dragging: false,
            drag_start: (0, 0, 0, 0),
            hover_btn: -1,
            btn_rects: [(0.0, 0.0, 0.0, 0.0); 4],
            curw: PILL_MINW,
            curh: pillh,
            anim: 0.0,
            anim_vel: 0.0,
            dir: 0,
            animating: false,
            mapped: false,
            media: Media::default(),
            have_media: false,
            targetw: PILL_MINW,
            art: None,
            loaded_art: String::new(),
            icon: None,
            loaded_icon: String::new(),
            pos_base_us: 0,
            pos_base_ms: 0,
            player: String::new(),
            net_active: atom("_NET_ACTIVE_WINDOW"),
            net_wm_state: atom("_NET_WM_STATE"),
            net_fullscreen: atom("_NET_WM_STATE_FULLSCREEN"),
            gc: xlib::XCreateGC(dpy, win, 0, ptr::null_mut()),
            buf: 0,
            bufw: 0,
            bufh: 0,
            sbuf: 0,
            sbufw: 0,
            sbufh: 0,
            static_key: 0,
            bar_levels: [0.0; MAX_BARS],
            bands,
            audio_active,
            art_tx,
            art_rx,
        })
    }

    fn place_window(&self) {
        let x = (self.sw - self.curw) / 2 + self.x_offset;
        unsafe {
            xlib::XMoveResizeWindow(
                self.dpy,
                self.win,
                x,
                self.topmargin,
                self.curw as u32,
                self.curh as u32,
            );
        }
    }

    fn art_size(&self) -> f64 {
        self.art_w.clamp(18.0, 96.0)
    }

    fn min_pill_height(&self) -> i32 {
        let tallest = self
            .art_h
            .max(self.text_h)
            .max(self.time_h)
            .max(self.spec_h)
            .max(self.btn_h);
        ((tallest + 6.0) as i32).max(30)
    }

    fn display_title(&self) -> &str {
        if !self.media.title.is_empty() {
            &self.media.title
        } else if self.edit_mode {
            "Edytujesz wyspę"
        } else {
            "Now playing"
        }
    }

    fn display_artist(&self) -> String {
        if self.edit_mode {
            match self.edit_sel {
                0 => format!("anim {:.1}s • okładka • kółko = szer. • Alt = wys.", self.anim_secs),
                1 => format!("anim {:.1}s • tekst • kółko = szer. • Alt = wys.", self.anim_secs),
                2 => format!("anim {:.1}s • czas • kółko = szer. • Alt = wys.", self.anim_secs),
                3 => format!("anim {:.1}s • fale • kółko = szer. • Alt = wys.", self.anim_secs),
                4 => format!("anim {:.1}s • przyciski • kółko = szer. • Alt = wys.", self.anim_secs),
                5 => format!(
                    "anim {:.1}s • główny box • kółko = wys. • Shift = szer. • Alt = wys.",
                    self.anim_secs
                ),
                _ => format!(
                    "anim {:.1}s • wybierz box • kółko = wys. • Shift = szer. • Ctrl = anim",
                    self.anim_secs
                ),
            }
        } else {
            self.media.artist.clone()
        }
    }

    fn button_rects(&self, w: f64, h: f64) -> [(f64, f64, f64, f64); 4] {
        let bw = self.btn_w;
        let bh = self.btn_h.clamp(10.0, 48.0);
        let total = 4.0 * self.btn_w + 3.0 * self.btn_gap;
        let x0 = w - PAD - self.pad_x - total;
        let y = (h - bh) / 2.0;
        [
            (x0, y, bw, bh),
            (x0 + self.btn_w + self.btn_gap, y, bw, bh),
            (x0 + 2.0 * (self.btn_w + self.btn_gap), y, bw, bh),
            (x0 + 3.0 * (self.btn_w + self.btn_gap), y, bw, bh),
        ]
    }

    fn draw_edit(&self) {
        unsafe {
            let raw = cairo::ffi::cairo_xlib_surface_create(self.dpy, self.editbuf, self.vis, 26, 26);
            let surface = match Surface::from_raw_full(raw) {
                Ok(s) => s,
                Err(_) => return,
            };
            let cr = match Context::new(&surface) {
                Ok(c) => c,
                Err(_) => return,
            };
            cr.set_operator(Operator::Source);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
            let _ = cr.paint();
            cr.set_operator(Operator::Over);

            rounded_rect(&cr, 0.5, 0.5, 25.0, 25.0, 8.0);
            if self.edit_mode {
                cr.set_source_rgba(0.48, 0.64, 0.97, 0.42);
            } else if self.edit_hover {
                cr.set_source_rgba(0.48, 0.64, 0.97, 0.28);
            } else {
                cr.set_source_rgba(0.035, 0.04, 0.06, 0.62);
            }
            let _ = cr.fill_preserve();
            cr.set_line_width(1.0);
            cr.set_source_rgba(0.48, 0.64, 0.97, 0.55);
            let _ = cr.stroke();

            // sliders icon
            let rows = [(7.5f64, 16.0f64), (13.0, 10.0), (18.5, 14.0)];
            for (y, kx) in rows {
                cr.set_line_width(1.5);
                cr.set_source_rgba(0.88, 0.90, 0.96, 0.9);
                cr.move_to(6.0, y);
                cr.line_to(20.0, y);
                let _ = cr.stroke();
                cr.arc(kx, y, 2.6, 0.0, 2.0 * std::f64::consts::PI);
                cr.set_source_rgba(0.48, 0.64, 0.97, 0.95);
                let _ = cr.fill();
            }
            let _ = surface.flush();
            xlib::XCopyArea(self.dpy, self.editbuf, self.editwin, self.gc, 0, 0, 26, 26, 0, 0);
            xlib::XFlush(self.dpy);
        }
    }

    fn set_edit_mode(&mut self, on: bool) {
        self.edit_mode = on;
        self.dragging = false;
        self.edit_sel = -1;
        if on {
            self.targetw = self.measure_target();
            self.curw = self.targetw;
            self.curh = self.pillh;
            if !self.mapped {
                self.anim = 0.0;
            }
            self.place_window();
            self.show();
            self.draw();
        } else {
            self.save_config();
        }
        self.draw_edit();
    }

    fn element_boxes(&self, w: f64, h: f64) -> [(f64, f64, f64, f64); 5] {
        let art_w = self.art_size();
        let art_h = self.art_h.clamp(18.0, 120.0);
        let text_x = PAD + self.pad_x + art_w + 10.0;
        let btn_area = 4.0 * self.btn_w + 3.0 * self.btn_gap;
        let buttons_x = w - PAD - self.pad_x - btn_area;
        let spec_x = buttons_x - 10.0 - self.spec_w;
        let time_x = spec_x - 8.0 - self.time_w;
        let text_right = time_x - 8.0;
        let text_h = self.text_h.clamp(16.0, 90.0);
        let time_h = self.time_h.clamp(10.0, 70.0);
        let spec_h = self.spec_h.clamp(10.0, 90.0);
        let btn_h = self.btn_h.clamp(10.0, 48.0);
        [
            (PAD + self.pad_x, (h - art_h) / 2.0, art_w, art_h),
            (
                text_x - 2.0,
                (h - text_h) / 2.0,
                (text_right - text_x + 2.0).max(10.0),
                text_h,
            ),
            (time_x, (h - time_h) / 2.0, self.time_w, time_h),
            (spec_x, (h - spec_h) / 2.0, self.spec_w, spec_h),
            (buttons_x, (h - btn_h) / 2.0, btn_area, btn_h),
        ]
    }

    fn rand_u64(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }

    fn widget_text_size(&self, text: &str) -> (i32, i32) {
        let fs = self.widget_font_size.clamp(12.0, 26.0);
        let fam = font_for_text(text, WIDGET_FONTS[self.widget_font.min(WIDGET_FONTS.len() - 1)]);
        unsafe {
            let pm = xlib::XCreatePixmap(self.dpy, self.widgetwin, 8, 8, 32);
            let tw = {
                let raw = cairo::ffi::cairo_xlib_surface_create(self.dpy, pm, self.vis, 8, 8);
                if let Ok(surface) = Surface::from_raw_full(raw) {
                    if let Ok(cr) = Context::new(&surface) {
                        set_font_ex(&cr, fam, fs, self.widget_bold, self.widget_italic);
                        text_width(&cr, text)
                    } else {
                        120.0
                    }
                } else {
                    120.0
                }
            };
            xlib::XFreePixmap(self.dpy, pm);
            let pad = 34 + if self.widget_3d { self.widget_depth * 3 } else { 0 };
            let h = (fs * 2.2 + 16.0) as i32;
            (tw as i32 + pad, h)
        }
    }

    fn ensure_widget_buffer(&mut self) {
        if self.widgetbuf == 0
            || self.widget_bufw != self.widget_w
            || self.widget_bufh != self.widget_h
        {
            unsafe {
                if self.widgetbuf != 0 {
                    xlib::XFreePixmap(self.dpy, self.widgetbuf);
                }
                self.widgetbuf = xlib::XCreatePixmap(
                    self.dpy,
                    self.widgetwin,
                    self.widget_w.max(1) as u32,
                    self.widget_h.max(1) as u32,
                    32,
                );
                self.widget_bufw = self.widget_w;
                self.widget_bufh = self.widget_h;
            }
        }
    }

    fn draw_widget(&mut self, phase: f64) {
        let w = self.widget_w as f64;
        let h = self.widget_h as f64;
        let a = if phase < 0.2 {
            (phase / 0.2).clamp(0.0, 1.0)
        } else if phase > 0.8 {
            ((1.0 - phase) / 0.2).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let (ac_r, ac_g, ac_b) = (
            DEFAULT_ACCENT[0] as f64,
            DEFAULT_ACCENT[1] as f64,
            DEFAULT_ACCENT[2] as f64,
        );
        unsafe {
            let raw = cairo::ffi::cairo_xlib_surface_create(
                self.dpy,
                self.widgetbuf,
                self.vis,
                self.widget_bufw.max(1),
                self.widget_bufh.max(1),
            );
            let surface = match Surface::from_raw_full(raw) {
                Ok(s) => s,
                Err(_) => return,
            };
            let cr = match Context::new(&surface) {
                Ok(c) => c,
                Err(_) => return,
            };
            cr.set_operator(Operator::Source);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
            let _ = cr.paint();
            cr.set_operator(Operator::Over);

            let fs = self.widget_font_size.clamp(12.0, 26.0);
            let fam = font_for_text(
                &self.widget_text,
                WIDGET_FONTS[self.widget_font.min(WIDGET_FONTS.len() - 1)],
            );
            set_font_ex(&cr, fam, fs, self.widget_bold, self.widget_italic);
            let label = ellipsize(&cr, &self.widget_text.clone(), w - 26.0);
            let tw = text_width(&cr, &label);
            let angle = if self.widget_3d {
                self.widget_angle + ((phase - 0.5) * 0.15)
            } else {
                0.0
            };
            let scale = if self.widget_3d {
                1.0 + 0.035 * ((phase * std::f64::consts::PI).sin())
            } else {
                1.0
            };
            let (cr_r, cr_g, cr_b) = (
                self.widget_color[0],
                self.widget_color[1],
                self.widget_color[2],
            );
            let _ = cr.save();
            cr.translate(w / 2.0, h / 2.0 + fs * 0.35);
            cr.rotate(angle);
            cr.scale(scale, scale);
            if self.widget_3d {
                for d in (1..=self.widget_depth).rev() {
                    cr.set_source_rgba(cr_r * 0.35, cr_g * 0.35, cr_b * 0.45, 0.55 * a);
                    cr.move_to(-tw / 2.0 + d as f64 * 0.8, d as f64 * 0.8);
                    let _ = cr.show_text(&label);
                }
            } else {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.5 * a);
                cr.move_to(-tw / 2.0 + 1.0, 1.0);
                let _ = cr.show_text(&label);
            }
            cr.set_source_rgba(cr_r, cr_g, cr_b, 0.96 * a);
            cr.move_to(-tw / 2.0, 0.0);
            let _ = cr.show_text(&label);
            let _ = cr.restore();

            let _ = surface.flush();
            xlib::XCopyArea(
                self.dpy,
                self.widgetbuf,
                self.widgetwin,
                self.gc,
                0,
                0,
                self.widget_bufw.max(1) as u32,
                self.widget_bufh.max(1) as u32,
                0,
                0,
            );
            xlib::XFlush(self.dpy);
        }
    }

    fn show_word(&mut self, word: String, t: u128, dur: u128) {
        // random typography for this word (nice fonts only)
        let r1 = self.rand_u64();
        let r2 = self.rand_u64();
        let r3 = self.rand_u64();
        self.widget_font = if r1 % 4 == 0 { 1 } else { 0 }; // Noto Sans mostly
        self.widget_font_size = 18.0 + (r2 % 9) as f64;
        self.widget_bold = r3 % 5 != 0;
        self.widget_italic = (r3 >> 3) % 4 == 0;
        self.widget_color = WIDGET_COLORS[((r3 >> 6) as usize) % WIDGET_COLORS.len()];
        self.widget_depth = 2 + (r1 % 4) as i32;
        self.widget_angle = ((r2 % 1000) as f64 / 1000.0 - 0.5) * 0.24;

        let (mut w, h) = self.widget_text_size(&word);
        w = w.clamp(70, (self.sw - 40).max(70));
        let place = if self.widget_place >= 2 {
            (self.rand_u64() % 2) as u8
        } else {
            self.widget_place
        };
        let (x, y) = if place == 1 {
            let island_x = (self.sw - self.curw) / 2 + self.x_offset;
            let cx = island_x + self.curw / 2;
            let px = (cx - w / 2).clamp(6, (self.sw - w - 6).max(6));
            let mut py = self.topmargin - h - 8;
            if py < 4 {
                py = self.topmargin + self.pillh + 8;
            }
            (px, py.clamp(4, (self.sh - h - 4).max(4)))
        } else {
            let x_range = (self.sw - w - 40).max(1) as u64;
            let y_range = (self.sh - h - 160).max(1) as u64;
            (
                20 + (self.rand_u64() % x_range) as i32,
                100 + (self.rand_u64() % y_range) as i32,
            )
        };

        self.widget_text = word;
        self.widget_w = w;
        self.widget_h = h;
        self.widget_x = x;
        self.widget_y = y;
        self.widget_start = t;
        self.widget_display_ms = dur;
        self.widget_active = true;
        self.ensure_widget_buffer();
        unsafe {
            xlib::XMoveResizeWindow(self.dpy, self.widgetwin, x, y, w as u32, h as u32);
            XShapeCombineRectangles(
                self.dpy,
                self.widgetwin,
                2,
                0,
                0,
                ptr::null_mut(),
                0,
                0,
                0,
            );
            xlib::XMapRaised(self.dpy, self.widgetwin);
        }
        self.draw_widget(0.2);
    }

    fn show_widget(&mut self, t: u128) {
        let r = self.rand_u64();
        let pool: &[String] = if self.widget_source == 1 {
            &self.lyrics_words
        } else {
            &self.words
        };
        if pool.is_empty() {
            return;
        }
        let word = if self.widget_continuous {
            let w = pool[self.plain_idx % pool.len()].clone();
            self.plain_idx = (self.plain_idx + 1) % pool.len().max(1);
            w
        } else {
            let mut idx = (r as usize) % pool.len();
            if pool.len() > 1 && pool[idx] == self.last_word {
                idx = (idx + 1) % pool.len();
            }
            pool[idx].clone()
        };
        self.last_word = word.clone();

        let dur = if self.widget_continuous {
            (250 + (self.rand_u64() % 251)).min(self.widget_word_ms as u64) as u128
        } else {
            self.widget_duration
        };
        self.show_word(word, t, dur);
    }

    fn refresh_schedule(&mut self) {
        let mut out = Vec::with_capacity(self.lyrics_raw.len());
        let mut prev = 0i64;
        for (i, (raw_t, word)) in self.lyrics_raw.iter().enumerate() {
            let aligned = self.aligned.get(i).and_then(|v| *v);
            let t = aligned.unwrap_or(*raw_t + self.sync_offset_ms);
            let t = t.max(prev + 80);
            prev = t;
            out.push((t, word.clone()));
        }
        self.lyrics_timed = out;
    }

    fn load_alignment(&mut self, key: &str) {
        self.aligned = vec![None; self.lyrics_raw.len()];
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for b in key.bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        self.align_path = cache_dir()
            .map(|d| d.join(format!("align-{hash:016x}.txt")).to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Ok(text) = fs::read_to_string(&self.align_path) {
            for line in text.lines() {
                let mut parts = line.splitn(3, '\t');
                let (Some(idx), Some(mmss_v), Some(_word)) =
                    (parts.next(), parts.next(), parts.next())
                else {
                    continue;
                };
                let Ok(i) = idx.parse::<usize>() else { continue };
                let Some(ms) = parse_mmss(mmss_v) else { continue };
                if i < self.aligned.len() {
                    self.aligned[i] = Some(ms);
                }
            }
        }
        self.refresh_schedule();
    }

    fn save_alignment(&self, index: usize) {
        if self.align_path.is_empty() || index >= self.lyrics_raw.len() {
            return;
        }
        if let Some(dir) = std::path::Path::new(&self.align_path).parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Some(t) = self.aligned.get(index).and_then(|v| *v) {
            let word = &self.lyrics_raw[index].1;
            let line = format!("{index}\t{}\t{word}\n", mmss(t));
            let _ = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.align_path)
                .and_then(|mut f| f.write_all(line.as_bytes()));
        }
    }

    fn handle_asr_word(&mut self, word: &str) {
        if self.lyrics_timed.is_empty() {
            return;
        }
        let spoken = norm_word(word);
        if spoken.len() < 2 {
            return;
        }
        let start = self.schedule_cursor.saturating_sub(16);
        let end = (self.schedule_cursor + 24).min(self.lyrics_timed.len());
        let mut found = None;
        for j in start..end {
            let expected = norm_word(&self.lyrics_timed[j].1);
            if similar(&spoken, &expected) {
                found = Some(j);
                break;
            }
        }
        if let Some(j) = found {
            let pos = (self.current_pos_ms() - 500).max(0);
            let expected_at = self.lyrics_timed[j].0;
            self.schedule_cursor = (j + 1).min(self.lyrics_timed.len());
            if j < self.aligned.len() {
                self.aligned[j] = Some(pos);
                self.refresh_schedule();
                self.save_alignment(j);
            }
            self.observe_drift(expected_at, pos);
            let shown = self.lyrics_timed[j].1.clone();
            if self.waterfall {
                self.spawn_fall_word(shown, now_ms());
            } else {
                self.show_word(shown, now_ms(), 400);
            }
        }
    }

    fn observe_drift(&mut self, expected_ms: i64, pos_ms: i64) {
        if !self.antidesync || self.lyrics_raw.is_empty() {
            return;
        }
        let drift = (pos_ms - expected_ms) as f64;
        self.drift_ema = if self.drift_n == 0 {
            drift
        } else {
            self.drift_ema * 0.7 + drift * 0.3
        };
        self.drift_n += 1;
        if self.drift_n >= 6 && self.drift_ema.abs() > self.max_drift_ms as f64 {
            let corr = (-self.drift_ema).round() as i64;
            self.sync_offset_ms = (self.sync_offset_ms + corr).clamp(-10_000, 10_000);
            self.refresh_schedule();
            self.drift_ema = 0.0;
            self.drift_n = 0;
        }
    }

    fn current_pos_ms(&self) -> i64 {
        let mut pos = self.pos_base_us;
        if self.media.playing == 1 {
            pos += (now_ms().saturating_sub(self.pos_base_ms)) as i64 * 1000;
        }
        (pos / 1000).max(0)
    }

    fn update_synced(&mut self, t: u128) {
        if self.media.playing != 1 {
            if self.widget_active {
                unsafe { xlib::XUnmapWindow(self.dpy, self.widgetwin) };
                self.widget_active = false;
            }
            return;
        }
        if self.widget_active {
            let elapsed = t.saturating_sub(self.widget_start);
            if elapsed >= self.widget_display_ms {
                unsafe { xlib::XUnmapWindow(self.dpy, self.widgetwin) };
                self.widget_active = false;
            } else {
                let phase =
                    (elapsed as f64 / self.widget_display_ms.max(1) as f64).clamp(0.0, 1.0);
                unsafe {
                    xlib::XMoveWindow(
                        self.dpy,
                        self.widgetwin,
                        self.widget_x,
                        self.widget_y - (phase * 8.0) as i32,
                    );
                }
                self.draw_widget(phase);
            }
        }
        let pos = self.current_pos_ms();
        if self.lyrics_timed.is_empty() || pos < self.lyrics_timed[0].0 {
            return;
        }
        let idx = match self
            .lyrics_timed
            .binary_search_by_key(&pos, |(at, _)| *at)
        {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        if idx >= self.lyrics_timed.len() {
            return;
        }
        // the invisible timeline lives in memory; the cursor never drifts
        if (idx + 1) < self.schedule_cursor {
            self.schedule_cursor = idx; // seeked back
        }
        if self.schedule_cursor > idx {
            return;
        }
        let mut last: Option<(i64, String)> = None;
        while self.schedule_cursor <= idx {
            let (at, word) = &self.lyrics_timed[self.schedule_cursor];
            last = Some((*at, word.clone()));
            self.schedule_cursor += 1;
        }
        if let Some((at, word)) = last {
            let next_at = self
                .lyrics_timed
                .get(self.schedule_cursor)
                .map(|x| x.0)
                .unwrap_or(pos + 500);
            let mut dur = 250 + (self.rand_u64() % 251) as i64; /* 250..500 ms */
            dur = dur.min((next_at - pos).max(120));
            dur = dur.min(self.widget_word_ms as i64).max(120);
            let _ = at;
            self.show_word(word, t, dur as u128);
        }
    }

    fn widget_pool_ready(&self) -> bool {
        if self.widget_source == 1 {
            !self.lyrics_words.is_empty() && self.media.playing == 1
        } else {
            !self.words.is_empty()
        }
    }

    fn update_widget(&mut self, t: u128) {
        if !self.widgets_enabled {
            return;
        }
        if self.waterfall {
            if self.widget_active {
                unsafe { xlib::XUnmapWindow(self.dpy, self.widgetwin) };
                self.widget_active = false;
            }
            return;
        }
        if !self.display_ready && self.current_pos_ms() >= self.warmup_ms {
            self.display_ready = true;
        }
        if !self.display_ready {
            if self.widget_active {
                unsafe { xlib::XUnmapWindow(self.dpy, self.widgetwin) };
                self.widget_active = false;
            }
            return;
        }
        if self.widget_continuous
            && self.widget_sync
            && self.lyrics_synced
            && !self.lyrics_timed.is_empty()
        {
            self.update_synced(t);
            return;
        }
        if !self.widget_active {
            if self.widget_continuous {
                if self.widget_pool_ready() {
                    self.show_widget(t);
                }
            } else if t >= self.widget_next {
                self.show_widget(t);
            }
            return;
        }
        let dur = if self.widget_continuous {
            self.widget_display_ms
        } else {
            self.widget_duration
        };
        let elapsed = t.saturating_sub(self.widget_start);
        let phase = (elapsed as f64 / dur as f64).clamp(0.0, 1.0);
        unsafe {
            xlib::XMoveWindow(
                self.dpy,
                self.widgetwin,
                self.widget_x,
                self.widget_y - (phase * 8.0) as i32,
            );
        }
        self.draw_widget(phase);
        if phase >= 1.0 {
            unsafe { xlib::XUnmapWindow(self.dpy, self.widgetwin) };
            self.widget_active = false;
            if self.widget_continuous {
                if self.widget_pool_ready() {
                    self.show_widget(t);
                }
            } else {
                let span = self.widget_max.saturating_sub(self.widget_min) + 1;
                let secs = self.widget_min + self.rand_u64() % span.max(1);
                self.widget_next = t + secs as u128 * 1000;
            }
        }
    }

    fn ensure_fall_buffer(&mut self) {
        if self.fallbuf == 0
            || self.fall_bufw != self.fall_w
            || self.fall_bufh != self.fall_h
        {
            unsafe {
                if self.fallbuf != 0 {
                    xlib::XFreePixmap(self.dpy, self.fallbuf);
                }
                self.fallbuf = xlib::XCreatePixmap(
                    self.dpy,
                    self.fallwin,
                    self.fall_w.max(1) as u32,
                    self.fall_h.max(1) as u32,
                    32,
                );
                self.fall_bufw = self.fall_w;
                self.fall_bufh = self.fall_h;
            }
        }
    }

    fn spawn_fall_word(&mut self, word: String, t: u128) {
        let r1 = self.rand_u64();
        let r2 = self.rand_u64();
        let r3 = self.rand_u64();
        let font = if r1 % 4 == 0 { 1 } else { 0 };
        let size = 14.0 + (r2 % 7) as f64;
        let color = WIDGET_COLORS[((r3 >> 6) as usize) % WIDGET_COLORS.len()];
        let island_x = (self.sw - self.curw) / 2 + self.x_offset;
        let cx = island_x + self.curw / 2 - self.fall_x0;
        let span = (self.curw.max(80) / 2) as u64;
        let jitter = (self.rand_u64() % span.max(1)) as i32 - span as i32 / 2;
        let x = (cx + jitter) as f64;
        let y = 4.0;
        let vx = -55.0 + (self.rand_u64() % 111) as f64;
        let vy = 110.0 + (self.rand_u64() % 140) as f64;
        let spin = ((self.rand_u64() % 1000) as f64 / 1000.0 - 0.5) * 1.4;

        if self.fall_words.len() >= 12 {
            self.fall_words.remove(0);
        }
        self.fall_words.push(FallWord {
            text: word,
            x,
            y,
            vx,
            vy,
            angle: 0.0,
            spin,
            font,
            size,
            color,
            bold: r3 % 5 != 0,
            italic: (r3 >> 3) % 4 == 0,
            depth: 1,
            born: t,
            life: 2200,
        });
    }

    fn draw_fall(&mut self, t: u128) {
        let (w, h) = (self.fall_w.max(1), self.fall_h.max(1));
        unsafe {
            let raw = cairo::ffi::cairo_xlib_surface_create(
                self.dpy,
                self.fallbuf,
                self.vis,
                w,
                h,
            );
            let surface = match Surface::from_raw_full(raw) {
                Ok(s) => s,
                Err(_) => return,
            };
            let cr = match Context::new(&surface) {
                Ok(c) => c,
                Err(_) => return,
            };
            cr.set_operator(Operator::Source);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
            let _ = cr.paint();
            cr.set_operator(Operator::Over);

            for word in &self.fall_words {
                let age = t.saturating_sub(word.born);
                let alpha = if age < 180 {
                    age as f64 / 180.0
                } else if age + 500 > word.life {
                    ((word.life.saturating_sub(age)) as f64 / 500.0).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                if alpha <= 0.0 {
                    continue;
                }
                if self.trails {
                    for k in 1..=1 {
                        let gx = word.x - word.vx * 0.06 * k as f64;
                        let gy = word.y - word.vy * 0.06 * k as f64;
                        let ga = alpha * 0.32 / k as f64;
                        draw_fall_text(&cr, word, gx, gy, word.angle - word.spin * 0.06 * k as f64, ga);
                    }
                }
                draw_fall_text(&cr, word, word.x, word.y, word.angle, alpha);
            }

            let _ = surface.flush();
            xlib::XCopyArea(
                self.dpy,
                self.fallbuf,
                self.fallwin,
                self.gc,
                0,
                0,
                w as u32,
                h as u32,
                0,
                0,
            );
            xlib::XFlush(self.dpy);
        }
    }

    fn update_fall(&mut self, t: u128) {
        if !self.waterfall {
            return;
        }
        if !self.display_ready && self.current_pos_ms() >= self.warmup_ms {
            self.display_ready = true;
        }
        if !self.display_ready {
            if self.fall_mapped {
                unsafe { xlib::XUnmapWindow(self.dpy, self.fallwin) };
                self.fall_mapped = false;
            }
            self.fall_words.clear();
            return;
        }
        let ready = self.media.playing == 1
            && if self.widget_source == 1 {
                !self.lyrics_words.is_empty()
            } else {
                !self.words.is_empty()
            };
        if !ready {
            if self.fall_mapped {
                unsafe { xlib::XUnmapWindow(self.dpy, self.fallwin) };
                self.fall_mapped = false;
            }
            self.fall_words.clear();
            return;
        }
        let island_x = (self.sw - self.curw) / 2 + self.x_offset;
        let cw = 560.min(self.sw).max(200);
        let cx = (island_x + self.curw / 2 - cw / 2).clamp(0, (self.sw - cw).max(0));
        let cy = (self.topmargin + self.pillh).clamp(0, (self.sh - 120).max(0));
        let ch = (self.sh - cy).clamp(120, 760);
        if !self.fall_mapped || self.fall_x0 != cx || self.fall_y0 != cy || self.fall_w != cw
            || self.fall_h != ch
        {
            self.fall_x0 = cx;
            self.fall_y0 = cy;
            self.fall_w = cw;
            self.fall_h = ch;
            self.fall_words.clear();
            self.fall_last_tick = t;
            unsafe {
                xlib::XMoveResizeWindow(
                    self.dpy,
                    self.fallwin,
                    cx,
                    cy,
                    cw as u32,
                    ch as u32,
                );
                xlib::XMapRaised(self.dpy, self.fallwin);
            }
            self.fall_mapped = true;
        }
        self.ensure_fall_buffer();
        if t.saturating_sub(self.fall_last_draw) < 50 {
            return;
        }
        self.fall_last_draw = t;

        // spawn new words
        if self.widget_sync && self.lyrics_synced && !self.lyrics_timed.is_empty() {
            let pos = self.current_pos_ms();
            if pos >= self.lyrics_timed[0].0 {
                let idx = match self
                    .lyrics_timed
                    .binary_search_by_key(&pos, |(at, _)| *at)
                {
                    Ok(i) => i,
                    Err(i) => i.saturating_sub(1),
                };
                if idx < self.lyrics_timed.len() {
                    if (idx + 1) < self.schedule_cursor {
                        self.schedule_cursor = idx;
                    }
                    while self.schedule_cursor <= idx {
                        let word = self.lyrics_timed[self.schedule_cursor].1.clone();
                        self.spawn_fall_word(word, t);
                        self.schedule_cursor += 1;
                    }
                }
            }
        } else if t >= self.fall_next {
            let pool: Vec<String> = if self.widget_source == 1 {
                self.lyrics_words.clone()
            } else {
                self.words.clone()
            };
            if !pool.is_empty() {
                let word = pool[self.plain_idx % pool.len()].clone();
                self.plain_idx = (self.plain_idx + 1) % pool.len().max(1);
                self.spawn_fall_word(word, t);
                self.fall_next = t + 250 + (self.rand_u64() % 251) as u128;
            }
        }

        let dt = if self.fall_last_tick == 0 {
            0.0
        } else {
            ((t.saturating_sub(self.fall_last_tick)) as f64 / 1000.0).min(0.1)
        };
        self.fall_last_tick = t;
        for word in &mut self.fall_words {
            word.x += word.vx * dt;
            word.y += word.vy * dt;
            word.angle += word.spin * dt;
        }
        let max_y = (self.sh + 80) as f64;
        self.fall_words
            .retain(|word| t.saturating_sub(word.born) < word.life && word.y < max_y);

        self.draw_fall(t);
    }

    fn save_config(&self) {
        let path = config_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let data = format!(
            "# Gabes Island layout\ntop={}\nheight={}\nx_offset={}\nextra_width={}\nart_w={}\nart_h={}\ntext_h={}\ntime_w={}\ntime_h={}\nspec_w={}\nspec_h={}\nbtn_w={}\nbtn_h={}\nbtn_gap={}\nanim_secs={}\npad_x={}\nwidgets={}\nwidget_min={}\nwidget_max={}\nwidget_source={}\nwidget_place={}\nwidget_3d={}\nwidget_continuous={}\nwidget_word_ms={}\nwidget_sync={}\nwidget_waterfall={}\nwidget_trails={}\nwidget_bpm={}\nwidget_antidesync={}\nwidget_max_drift_ms={}\nasr_enabled={}\nasr_python={}\nasr_script={}\nasr_model={}\nwidget_warmup_ms={}\n",
            self.topmargin,
            self.pillh,
            self.x_offset,
            self.extra_width,
            self.art_w,
            self.art_h,
            self.text_h,
            self.time_w,
            self.time_h,
            self.spec_w,
            self.spec_h,
            self.btn_w,
            self.btn_h,
            self.btn_gap,
            self.anim_secs,
            self.pad_x,
            if self.widgets_enabled { 1 } else { 0 },
            self.widget_min,
            self.widget_max,
            if self.widget_source == 1 { "lyrics" } else { "words" },
            match self.widget_place {
                1 => "island",
                2 => "both",
                _ => "random",
            },
            if self.widget_3d { 1 } else { 0 },
            if self.widget_continuous { 1 } else { 0 },
            self.widget_word_ms,
            if self.widget_sync { 1 } else { 0 },
            if self.waterfall { 1 } else { 0 },
            if self.trails { 1 } else { 0 },
            self.widget_bpm,
            if self.antidesync { 1 } else { 0 },
            self.max_drift_ms,
            if self.asr_enabled { 1 } else { 0 },
            "~/.local/share/gabes-island/venv/bin/python",
            "~/.local/share/gabes-island/asr_vosk.py",
            "~/.local/share/gabes-island/vosk-model-small-en-us-0.15",
            self.warmup_ms
        );
        let _ = std::fs::write(path, data);
    }

    fn measure_target(&self) -> i32 {
        unsafe {
            let pm = xlib::XCreatePixmap(self.dpy, self.win, 8, 8, 32);
            let res = {
                let raw = cairo::ffi::cairo_xlib_surface_create(self.dpy, pm, self.vis, 8, 8);
                if let Ok(surface) = Surface::from_raw_full(raw) {
                    if let Ok(cr) = Context::new(&surface) {
                        // width comes from the track, not from the edit hint
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
                        let tscale = (self.text_h / 26.0).clamp(0.6, 2.0);
                        set_font_ex(
                            &cr,
                            font_for_text(title, "Noto Sans"),
                            15.0 * tscale,
                            true,
                            false,
                        );
                        let t = ellipsize(&cr, title, 460.0);
                        let tw = text_width(&cr, &t);
                        set_font_ex(
                            &cr,
                            font_for_text(artist, "Noto Sans"),
                            12.0 * tscale,
                            false,
                            false,
                        );
                        let a = ellipsize(&cr, artist, 460.0);
                        let aw = text_width(&cr, &a);
                        let textw = tw.max(aw);
                        let btn_area = 4.0 * self.btn_w + 3.0 * self.btn_gap;
                        let w = PAD
                            + self.pad_x
                            + self.art_size()
                            + 10.0
                            + textw
                            + self.extra_width
                            + 8.0
                            + self.time_w
                            + 8.0
                            + self.spec_w
                            + 10.0
                            + btn_area
                            + self.pad_x
                            + PAD;
                        (w as i32).clamp(PILL_MINW, PILL_MAXW)
                    } else {
                        PILL_MINW
                    }
                } else {
                    PILL_MINW
                }
            };
            xlib::XFreePixmap(self.dpy, pm);
            res
        }
    }

    fn ensure_buffer(&mut self) {
        if self.buf == 0 || self.bufw != self.curw || self.bufh != self.curh {
            unsafe {
                if self.buf != 0 {
                    xlib::XFreePixmap(self.dpy, self.buf);
                }
                if self.sbuf != 0 {
                    xlib::XFreePixmap(self.dpy, self.sbuf);
                }
                self.buf = xlib::XCreatePixmap(
                    self.dpy,
                    self.win,
                    self.curw.max(1) as u32,
                    self.curh.max(1) as u32,
                    32,
                );
                self.sbuf = xlib::XCreatePixmap(
                    self.dpy,
                    self.win,
                    self.curw.max(1) as u32,
                    self.curh.max(1) as u32,
                    32,
                );
                self.bufw = self.curw;
                self.bufh = self.curh;
                self.sbufw = self.curw;
                self.sbufh = self.curh;
                self.static_key = 0;
            }
        }
    }

    fn static_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.curw.hash(&mut h);
        self.curh.hash(&mut h);
        self.pillh.hash(&mut h);
        for f in [
            self.art_w,
            self.art_h,
            self.text_h,
            self.time_w,
            self.time_h,
            self.spec_w,
            self.spec_h,
            self.btn_w,
            self.btn_h,
            self.btn_gap,
            self.pad_x,
            self.extra_width,
            self.anim,
        ] {
            f.to_bits().hash(&mut h);
        }
        self.edit_mode.hash(&mut h);
        self.edit_sel.hash(&mut h);
        self.hover_btn.hash(&mut h);
        self.media.playing.hash(&mut h);
        self.media.title.hash(&mut h);
        self.media.artist.hash(&mut h);
        self.media.art.hash(&mut h);
        self.loaded_icon.hash(&mut h);
        self.art.is_some().hash(&mut h);
        self.icon.is_some().hash(&mut h);
        h.finish()
    }

    fn dynamic_rects(&self) -> [(i32, i32, i32, i32); 3] {
        let w = self.curw as f64;
        let h = self.curh as f64;
        let art = self.art_size();
        let textx = PAD + self.pad_x + art + 10.0;
        let btn_area = 4.0 * self.btn_w + 3.0 * self.btn_gap;
        let buttons_x = w - PAD - self.pad_x - btn_area;
        let spec_x = buttons_x - 10.0 - self.spec_w;
        let time_x = spec_x - 8.0 - self.time_w;
        let text_right = time_x - 8.0;
        let spec_h = self.spec_h.clamp(10.0, 90.0);
        let spec_y0 = (h - spec_h) / 2.0;
        let time_h = self.time_h.clamp(10.0, 70.0);
        let time_y0 = (h - time_h) / 2.0;
        let clamp_rect = |x: f64, y: f64, ww: f64, hh: f64| -> (i32, i32, i32, i32) {
            let x0 = x.floor().max(0.0) as i32;
            let y0 = y.floor().max(0.0) as i32;
            let x1 = (x + ww).ceil().min(w) as i32;
            let y1 = (y + hh).ceil().min(h) as i32;
            (x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
        };
        [
            clamp_rect(spec_x - 1.0, spec_y0 - 2.0, self.spec_w + 2.0, spec_h + 4.0),
            clamp_rect(time_x - 2.0, time_y0 - 1.0, self.time_w + 4.0, time_h + 2.0),
            clamp_rect(textx, h - 9.0, text_right - textx, 9.0),
        ]
    }

    fn draw_dynamic(&mut self, cr: &Context) {
        let w = self.curw as f64;
        let h = self.curh as f64;
        let a = if self.anim > 0.0 { self.anim } else { 0.001 };
        let art = self.art_size();
        let textx = PAD + self.pad_x + art + 10.0;
        let btn_area = 4.0 * self.btn_w + 3.0 * self.btn_gap;
        let buttons_x = w - PAD - self.pad_x - btn_area;
        let spec_x = buttons_x - 10.0 - self.spec_w;
        let time_x = spec_x - 8.0 - self.time_w;
        let text_right = time_x - 8.0;
        let (ac_r, ac_g, ac_b) = (
            DEFAULT_ACCENT[0] as f64,
            DEFAULT_ACCENT[1] as f64,
            DEFAULT_ACCENT[2] as f64,
        );

        let incoming = self
            .bands
            .lock()
            .map(|g| *g)
            .unwrap_or([0.0; MAX_BARS]);
        for i in 0..MAX_BARS {
            self.bar_levels[i] *= 0.86;
            if incoming[i] > self.bar_levels[i] {
                self.bar_levels[i] = incoming[i];
            }
        }

        let mut pos = self.pos_base_us;
        if self.media.playing == 1 {
            pos += (now_ms().saturating_sub(self.pos_base_ms)) as i64 * 1000;
        }
        pos = pos.max(0);
        let progress = if self.media.length_us > 0 {
            (pos as f64 / self.media.length_us as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };

        set_font(cr, 9.0, false);
        let time = fmt_time(pos);
        cr.set_source_rgba(0.60, 0.64, 0.74, a);
        let tw = text_width(cr, &time);
        let time_hh = self.time_h.clamp(10.0, 70.0);
        cr.move_to(
            time_x + self.time_w - tw,
            (h - time_hh) / 2.0 + time_hh / 2.0 + 3.0,
        );
        let _ = cr.show_text(&time);

        let spec_area = (self.spec_w * self.spec_h).max(1.0);
        let nbars = ((spec_area / 140.0).round() as usize).clamp(4, MAX_BARS);
        let slot = self.spec_w / nbars as f64;
        let barw = (slot * 0.62).max(1.2);
        let spec_hh = self.spec_h.clamp(10.0, 90.0);
        let spec_y0 = (h - spec_hh) / 2.0;
        for i in 0..nbars {
            let band = i * MAX_BARS / nbars;
            let lvl = if self.media.playing == 1 {
                self.bar_levels[band] as f64
            } else {
                0.16
            };
            let bh = spec_hh * (0.16 + 0.84 * lvl);
            let bx = spec_x + i as f64 * slot;
            let by = spec_y0 + (spec_hh - bh) / 2.0;
            rounded_rect(cr, bx, by, barw, bh, 1.2);
            let alpha = if self.media.playing == 1 { 0.9 } else { 0.45 };
            cr.set_source_rgba(ac_r, ac_g, ac_b, alpha * a);
            let _ = cr.fill();
        }

        if self.media.length_us > 0 {
            let px0 = textx;
            let px1 = text_right;
            rounded_rect(cr, px0, h - 6.0, px1 - px0, 1.6, 0.8);
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.08 * a);
            let _ = cr.fill();
            rounded_rect(cr, px0, h - 6.0, (px1 - px0) * progress, 1.6, 0.8);
            cr.set_source_rgba(ac_r, ac_g, ac_b, 0.9 * a);
            let _ = cr.fill();
        }
    }

    fn draw(&mut self) {
        self.ensure_buffer();
        let key = self.static_key();
        let full = key != self.static_key;
        unsafe {
            if full {
                let raw = cairo::ffi::cairo_xlib_surface_create(
                    self.dpy,
                    self.sbuf,
                    self.vis,
                    self.sbufw,
                    self.sbufh,
                );
                if let Ok(surface) = Surface::from_raw_full(raw) {
                    if let Ok(cr) = Context::new(&surface) {
                        self.draw_on(&cr, true);
                        let _ = surface.flush();
                    }
                }
                xlib::XCopyArea(
                    self.dpy,
                    self.sbuf,
                    self.buf,
                    self.gc,
                    0,
                    0,
                    self.bufw as u32,
                    self.bufh as u32,
                    0,
                    0,
                );
                self.static_key = key;
            } else {
                for (x, y, w, h) in self.dynamic_rects() {
                    if w > 0 && h > 0 {
                        xlib::XCopyArea(
                            self.dpy,
                            self.sbuf,
                            self.buf,
                            self.gc,
                            x,
                            y,
                            w as u32,
                            h as u32,
                            x,
                            y,
                        );
                    }
                }
            }

            let raw = cairo::ffi::cairo_xlib_surface_create(
                self.dpy,
                self.buf,
                self.vis,
                self.bufw,
                self.bufh,
            );
            if let Ok(surface) = Surface::from_raw_full(raw) {
                if let Ok(cr) = Context::new(&surface) {
                    self.draw_dynamic(&cr);
                    let _ = surface.flush();
                }
            }

            if full {
                xlib::XCopyArea(
                    self.dpy,
                    self.buf,
                    self.win,
                    self.gc,
                    0,
                    0,
                    self.bufw as u32,
                    self.bufh as u32,
                    0,
                    0,
                );
            } else {
                for (x, y, w, h) in self.dynamic_rects() {
                    if w > 0 && h > 0 {
                        xlib::XCopyArea(
                            self.dpy,
                            self.buf,
                            self.win,
                            self.gc,
                            x,
                            y,
                            w as u32,
                            h as u32,
                            x,
                            y,
                        );
                    }
                }
            }
            xlib::XFlush(self.dpy);
        }
    }

    fn draw_on(&mut self, cr: &Context, skip_dynamic: bool) {
        let w = self.curw as f64;
        let h = self.curh as f64;
        let a = if self.anim > 0.0 { self.anim } else { 0.001 };

        let (ac_r, ac_g, ac_b) = (
            DEFAULT_ACCENT[0] as f64,
            DEFAULT_ACCENT[1] as f64,
            DEFAULT_ACCENT[2] as f64,
        );

        cr.set_operator(Operator::Source);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        let _ = cr.paint();
        cr.set_operator(Operator::Over);

        rounded_rect(cr, 0.0, 0.0, w, h, h / 2.0);
        cr.set_source_rgba(0.035, 0.04, 0.06, 0.88 * a);
        let _ = cr.fill_preserve();
        cr.set_line_width(1.0);
        cr.set_source_rgba(ac_r, ac_g, ac_b, 0.28 * a);
        let _ = cr.stroke();

        // album art
        let art_w = self.art_size();
        let art_h = self.art_h.clamp(18.0, 120.0);
        let ax = PAD + self.pad_x;
        let ay = (h - art_h) / 2.0;
        rounded_rect(cr, ax, ay, art_w, art_h, 6.0);
        let _ = cr.clip_preserve();
        if let Some(art) = &self.art {
            let iw = art.width() as f64;
            let ih = art.height() as f64;
            let s = (art_w / iw).min(art_h / ih);
            let dw = iw * s;
            let dh = ih * s;
            let _ = cr.save();
            cr.translate(ax + (art_w - dw) / 2.0, ay + (art_h - dh) / 2.0);
            cr.scale(s, s);
            let pat = cairo::SurfacePattern::create(art);
            pat.set_filter(cairo::Filter::Best);
            let _ = cr.set_source(&pat);
            let _ = cr.paint();
            let _ = cr.restore();
        } else if let Some(icon) = &self.icon {
            let iw = icon.width() as f64;
            let ih = icon.height() as f64;
            let box_w = art_w - 4.0;
            let box_h = art_h - 4.0;
            let s = (box_w / iw).min(box_h / ih);
            let dw = iw * s;
            let dh = ih * s;
            let _ = cr.save();
            cr.translate(ax + (art_w - dw) / 2.0, ay + (art_h - dh) / 2.0);
            cr.scale(s, s);
            let pat = cairo::SurfacePattern::create(icon);
            pat.set_filter(cairo::Filter::Best);
            let _ = cr.set_source(&pat);
            let _ = cr.paint();
            let _ = cr.restore();
        } else {
            let pat = cairo::LinearGradient::new(ax, ay, ax, ay + art_h);
            pat.add_color_stop_rgba(0.0, 0.30, 0.34, 0.44, a);
            pat.add_color_stop_rgba(1.0, 0.15, 0.17, 0.24, a);
            cr.set_source(&pat).ok();
            let _ = cr.fill();
            let k = art_w.min(art_h) / 32.0;
            let cx = ax + art_w / 2.0;
            let cy = ay + art_h / 2.0;
            let _ = cr.save();
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.9 * a);
            let _ = cr.save();
            cr.translate(cx - 4.0 * k, cy + 3.2 * k);
            cr.rotate(-0.35);
            cr.scale(1.0, 0.72);
            cr.arc(0.0, 0.0, 4.7 * k, 0.0, 2.0 * std::f64::consts::PI);
            let _ = cr.fill();
            let _ = cr.restore();
            cr.rectangle(cx - 1.2 * k, cy - 10.5 * k, 1.6 * k, 14.0 * k);
            let _ = cr.fill();
            cr.move_to(cx - 1.2 * k, cy - 10.5 * k);
            cr.curve_to(cx + 4.4 * k, cy - 9.3 * k, cx + 4.4 * k, cy - 4.8 * k, cx + 1.4 * k, cy - 2.4 * k);
            cr.curve_to(cx + 3.4 * k, cy - 6.0 * k, cx + 2.4 * k, cy - 8.4 * k, cx - 1.2 * k, cy - 8.4 * k);
            cr.close_path();
            let _ = cr.fill();
            let _ = cr.restore();
        }
        cr.reset_clip();
        rounded_rect(cr, ax + 0.5, ay + 0.5, art_w - 1.0, art_h - 1.0, 6.0);
        cr.set_line_width(1.0);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.12 * a);
        let _ = cr.stroke();

        let textx = PAD + self.pad_x + art_w + 10.0;
        let tscale = (self.text_h / 26.0).clamp(0.6, 2.0);
        let text_y0 = (h - self.text_h.clamp(16.0, 90.0)) / 2.0;
        let text_hh = self.text_h.clamp(16.0, 90.0);
        let title_base = text_y0 + text_hh * 0.42;
        let artist_base = text_y0 + text_hh * 0.92;
        let btns = self.button_rects(w, h);
        let btn_area = 4.0 * self.btn_w + 3.0 * self.btn_gap;
        let buttons_x = w - PAD - self.pad_x - btn_area;
        let spec_x = buttons_x - 10.0 - self.spec_w;
        let time_x = spec_x - 8.0 - self.time_w;
        let text_right = time_x - 8.0;
        let maxw = (text_right - textx - 4.0).max(10.0);
        let spec_area = (self.spec_w * self.spec_h).max(1.0);
        let nbars = ((spec_area / 140.0).round() as usize).clamp(4, MAX_BARS);
        let slot = self.spec_w / nbars as f64;
        let barw = (slot * 0.62).max(1.2);
        let bgap = (slot - barw).max(0.4);
        let bars_x = spec_x;

        // title
        let title = ellipsize(cr, self.display_title(), maxw);
        set_font_ex(
            cr,
            font_for_text(&title, "Noto Sans"),
            12.0 * tscale,
            true,
            false,
        );
        cr.set_source_rgba(0.90, 0.92, 0.97, a);
        cr.move_to(textx, title_base);
        let _ = cr.show_text(&title);

        // artist
        if !self.display_artist().is_empty() {
            let artist = ellipsize(cr, &self.display_artist(), maxw);
            set_font_ex(
                cr,
                font_for_text(&artist, "Noto Sans"),
                10.0 * tscale,
                false,
                false,
            );
            cr.set_source_rgba(0.56, 0.60, 0.70, a);
            cr.move_to(textx, artist_base);
            let _ = cr.show_text(&artist);
        }

        if !skip_dynamic {
        // elapsed + progress
        let mut pos = self.pos_base_us;
        if self.media.playing == 1 {
            let elapsed = (now_ms().saturating_sub(self.pos_base_ms)) as i64 * 1000;
            pos += elapsed;
        }
        pos = pos.max(0);
        let progress = if self.media.length_us > 0 {
            (pos as f64 / self.media.length_us as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };

        set_font(cr, 9.0, false);
        let time = fmt_time(pos);
        cr.set_source_rgba(0.60, 0.64, 0.74, a);
        let tw = text_width(cr, &time);
        let time_hh = self.time_h.clamp(10.0, 70.0);
        cr.move_to(
            time_x + self.time_w - tw,
            (h - time_hh) / 2.0 + time_hh / 2.0 + 3.0,
        );
        let _ = cr.show_text(&time);

        // spectrum: real band levels captured from the sink monitor
        let spec_hh = self.spec_h.clamp(10.0, 90.0);
        let spec_y0 = (h - spec_hh) / 2.0;
        for i in 0..nbars {
            let band = i * MAX_BARS / nbars;
            let lvl = if self.media.playing == 1 {
                self.bar_levels[band] as f64
            } else {
                0.16
            };
            let bh = spec_hh * (0.16 + 0.84 * lvl);
            let bx = bars_x + i as f64 * slot;
            let by = spec_y0 + (spec_hh - bh) / 2.0;
            rounded_rect(cr, bx, by, barw, bh, 1.2);
            let alpha = if self.media.playing == 1 { 0.9 } else { 0.45 };
            cr.set_source_rgba(ac_r, ac_g, ac_b, alpha * a);
            let _ = cr.fill();
        }

        // transport buttons: previous / play-pause / next / edit
        self.btn_rects = btns;
        for i in 0..4 {
            let (bx, by, bw, bh) = btns[i];
            let k = (bw.min(bh) / 18.0).clamp(0.5, 2.0);
            if self.hover_btn == i as i32 {
                rounded_rect(cr, bx, by, bw, bh, 6.0 * k);
                cr.set_source_rgba(ac_r, ac_g, ac_b, 0.22 * a);
                let _ = cr.fill();
            }
            let cx = bx + bw / 2.0;
            let cy = by + bh / 2.0;
            let col = if self.hover_btn == i as i32 {
                (ac_r, ac_g, ac_b)
            } else {
                (0.88, 0.90, 0.96)
            };
            cr.set_source_rgba(col.0, col.1, col.2, 0.95 * a);
            match i {
                0 => {
                    cr.rectangle(cx - 6.0 * k, cy - 5.0 * k, 2.0 * k, 10.0 * k);
                    let _ = cr.fill();
                    cr.move_to(cx + 5.0 * k, cy - 5.0 * k);
                    cr.line_to(cx + 5.0 * k, cy + 5.0 * k);
                    cr.line_to(cx - 3.0 * k, cy);
                    cr.close_path();
                    let _ = cr.fill();
                }
                1 => {
                    if self.media.playing == 1 {
                        cr.rectangle(cx - 4.0 * k, cy - 5.0 * k, 3.0 * k, 10.0 * k);
                        let _ = cr.fill();
                        cr.rectangle(cx + 1.0 * k, cy - 5.0 * k, 3.0 * k, 10.0 * k);
                        let _ = cr.fill();
                    } else {
                        cr.move_to(cx - 3.0 * k, cy - 6.0 * k);
                        cr.line_to(cx - 3.0 * k, cy + 6.0 * k);
                        cr.line_to(cx + 6.0 * k, cy);
                        cr.close_path();
                        let _ = cr.fill();
                    }
                }
                2 => {
                    cr.move_to(cx - 5.0 * k, cy - 5.0 * k);
                    cr.line_to(cx - 5.0 * k, cy + 5.0 * k);
                    cr.line_to(cx + 3.0 * k, cy);
                    cr.close_path();
                    let _ = cr.fill();
                    cr.rectangle(cx + 4.0 * k, cy - 5.0 * k, 2.0 * k, 10.0 * k);
                    let _ = cr.fill();
                }
                _ => {
                    for (yy, kx) in [(-5.0f64, 3.0f64), (0.0, -3.0), (5.0, 1.5)] {
                        cr.set_line_width(1.2 * k);
                        cr.move_to(cx - 5.0 * k, cy + yy * k);
                        cr.line_to(cx + 5.0 * k, cy + yy * k);
                        let _ = cr.stroke();
                        cr.arc(cx + kx * k, cy + yy * k, 1.7 * k, 0.0, 2.0 * std::f64::consts::PI);
                        let _ = cr.fill();
                    }
                }
            }
        }

        // progress line
        if self.media.length_us > 0 {
            let px0 = textx;
            let px1 = text_right;
            rounded_rect(cr, px0, h - 6.0, px1 - px0, 1.6, 0.8);
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.08 * a);
            let _ = cr.fill();
            rounded_rect(cr, px0, h - 6.0, (px1 - px0) * progress, 1.6, 0.8);
            cr.set_source_rgba(ac_r, ac_g, ac_b, 0.9 * a);
            let _ = cr.fill();
        }

        }

        if self.edit_mode {
            let boxes = self.element_boxes(w, h);
            for i in 0..5 {
                let b = boxes[i];
                let sel = self.edit_sel == i as i32;
                let _ = cr.set_dash(if sel { &[] } else { &[3.0, 3.0] }, 0.0);
                rounded_rect(cr, b.0, b.1, b.2, b.3, 4.0);
                cr.set_line_width(if sel { 1.6 } else { 1.0 });
                if sel {
                    cr.set_source_rgba(ac_r, ac_g, ac_b, 0.10);
                    let _ = cr.fill_preserve();
                }
                cr.set_source_rgba(ac_r, ac_g, ac_b, if sel { 0.95 } else { 0.45 });
                let _ = cr.stroke();
            }
            let _ = cr.set_dash(&[], 0.0);
            let main_sel = self.edit_sel == 5;
            let _ = cr.set_dash(if main_sel { &[] } else { &[4.0, 4.0] }, 0.0);
            rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, (h - 1.0) / 2.0);
            cr.set_line_width(if main_sel { 1.8 } else { 1.0 });
            cr.set_source_rgba(ac_r, ac_g, ac_b, if main_sel { 0.95 } else { 0.5 } * a);
            let _ = cr.stroke();
            let _ = cr.set_dash(&[], 0.0);
        }
    }

    fn show(&mut self) {
        self.anim_vel = 0.0;
        unsafe {
            if !self.mapped {
                xlib::XMapRaised(self.dpy, self.win);
                self.mapped = true;
            } else {
                xlib::XRaiseWindow(self.dpy, self.win);
            }
        }
        self.dir = 1;
        self.animating = true;
    }

    fn hide(&mut self) {
        if self.mapped {
            self.dir = -1;
            self.anim_vel = 0.0;
            self.animating = true;
        }
    }

    fn ensure_on_top(&self) {
        unsafe {
            let mut root_ret: xlib::Window = 0;
            let mut parent: xlib::Window = 0;
            let mut children: *mut xlib::Window = ptr::null_mut();
            let mut n: u32 = 0;
            if xlib::XQueryTree(
                self.dpy,
                self.root,
                &mut root_ret,
                &mut parent,
                &mut children,
                &mut n,
            ) != 0
            {
                if !children.is_null() {
                    let top1 = if n >= 1 { *children.add((n - 1) as usize) } else { 0 };
                    let top2 = if n >= 2 { *children.add((n - 2) as usize) } else { 0 };
                    if self.fall_mapped {
                        xlib::XRaiseWindow(self.dpy, self.fallwin);
                    }
                    if self.mapped {
                        // pill above the edit button, so the button hides under it
                        if top1 != self.win || top2 != self.editwin {
                            xlib::XRaiseWindow(self.dpy, self.editwin);
                            xlib::XRaiseWindow(self.dpy, self.win);
                        }
                    } else if top1 != self.editwin {
                        xlib::XRaiseWindow(self.dpy, self.editwin);
                    }
                    if self.widget_active {
                        xlib::XRaiseWindow(self.dpy, self.widgetwin);
                    }
                    xlib::XFree(children as *mut c_void);
                }
            }
        }
    }

    fn tick(&mut self, dt: f64) {
        // spring, the way the Dynamic Island animates: a little overshoot,
        // never a linear slide
        let target = if self.dir > 0 { 1.0 } else { 0.0 };
        let omega = 5.0 / self.anim_secs.max(0.2);
        let stiffness = omega * omega;
        let damping = 1.8 * omega;
        let accel = stiffness * (target - self.anim) - damping * self.anim_vel;
        self.anim_vel += accel * dt;
        self.anim += self.anim_vel * dt;
        if self.anim < 0.0 {
            self.anim = 0.0;
            if self.anim_vel < 0.0 {
                self.anim_vel = 0.0;
            }
        }
        if self.anim > 1.08 {
            self.anim = 1.08;
        }
        let settled = (target - self.anim).abs() < 0.004 && self.anim_vel.abs() < 0.03;
        if settled {
            self.anim = target;
            self.anim_vel = 0.0;
            self.animating = false;
        }
        if self.dir < 0 && self.anim <= 0.0001 && !self.animating {
            if self.mapped {
                unsafe { xlib::XUnmapWindow(self.dpy, self.win) };
                self.mapped = false;
            }
            self.bar_levels = [0.0; MAX_BARS];
            return;
        }
        let e = self.anim.clamp(0.0, 1.08);
        let w = 48.0 + (self.targetw as f64 - 48.0) * e;
        let h = 7.0 + (self.pillh as f64 - 7.0) * e;
        self.curw = w as i32;
        self.curh = h as i32;
        self.place_window();
        if self.mapped {
            self.draw();
        }
    }

    fn active_fullscreen(&self) -> bool {
        unsafe {
            let mut actual_type: xlib::Atom = 0;
            let mut actual_format: c_int = 0;
            let mut nitems: c_ulong = 0;
            let mut after: c_ulong = 0;
            let mut data: *mut c_uchar = ptr::null_mut();
            let mut aw: xlib::Window = 0;

            if xlib::XGetWindowProperty(
                self.dpy,
                self.root,
                self.net_active,
                0,
                1,
                xlib::False,
                xlib::XA_WINDOW,
                &mut actual_type,
                &mut actual_format,
                &mut nitems,
                &mut after,
                &mut data,
            ) == xlib::Success as c_int
                && !data.is_null()
            {
                if nitems > 0 {
                    aw = *(data as *mut xlib::Window);
                }
                xlib::XFree(data as *mut c_void);
            }

            // Cinnamon can leave _NET_ACTIVE_WINDOW unset; fall back to the
            // keyboard focus and walk up to the top-level window.
            if aw == 0 {
                let mut focus: xlib::Window = 0;
                let mut revert: c_int = 0;
                xlib::XGetInputFocus(self.dpy, &mut focus, &mut revert);
                let mut w = focus;
                while w != 0 && w != self.root {
                    let mut r: xlib::Window = 0;
                    let mut parent: xlib::Window = 0;
                    let mut children: *mut xlib::Window = ptr::null_mut();
                    let mut n: u32 = 0;
                    if xlib::XQueryTree(
                        self.dpy,
                        w,
                        &mut r,
                        &mut parent,
                        &mut children,
                        &mut n,
                    ) == 0
                    {
                        break;
                    }
                    if !children.is_null() {
                        xlib::XFree(children as *mut c_void);
                    }
                    if parent == self.root || parent == 0 {
                        break;
                    }
                    w = parent;
                }
                aw = w;
            }

            if aw == 0 {
                return false;
            }
            data = ptr::null_mut();
            let mut fs = false;
            if xlib::XGetWindowProperty(
                self.dpy,
                aw,
                self.net_wm_state,
                0,
                32,
                xlib::False,
                xlib::XA_ATOM,
                &mut actual_type,
                &mut actual_format,
                &mut nitems,
                &mut after,
                &mut data,
            ) == xlib::Success as c_int
                && !data.is_null()
            {
                let atoms = data as *mut xlib::Atom;
                for i in 0..nitems as usize {
                    if *atoms.add(i) == self.net_fullscreen {
                        fs = true;
                    }
                }
                xlib::XFree(data as *mut c_void);
            }
            fs
        }
    }
}

// ---------------------------------------------------------------- main

fn now_ms() -> u128 {
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis()
}

fn main() {
    let mut layout = load_layout();
    let args: Vec<String> = std::env::args().collect();
    let mut start_edit = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--top" if i + 1 < args.len() => {
                layout.top = args[i + 1].parse().unwrap_or(layout.top);
                i += 2;
            }
            "--height" if i + 1 < args.len() => {
                layout.height = args[i + 1].parse().unwrap_or(layout.height);
                i += 2;
            }
            "--extra-width" if i + 1 < args.len() => {
                layout.extra = args[i + 1].parse().unwrap_or(layout.extra);
                i += 2;
            }
            "--x-offset" if i + 1 < args.len() => {
                layout.xoff = args[i + 1].parse().unwrap_or(layout.xoff);
                i += 2;
            }
            "--edit" => {
                start_edit = true;
                i += 1;
            }
            _ => i += 1,
        }
    }

    let audio_active = Arc::new(AtomicBool::new(false));
    let (art_tx, art_rx) = mpsc::channel::<(String, String)>();
    let (lyrics_tx, lyrics_rx) = mpsc::channel::<(String, String)>();
    let (asr_tx, asr_rx) = mpsc::channel::<String>();
    let asr_child = Arc::new(Mutex::new(None));
    let asr_active = Arc::new(AtomicBool::new(false));
    let mut app = match unsafe {
        App::new(
            layout.clone(),
            spawn_audio(audio_active.clone()),
            audio_active.clone(),
            art_tx,
            art_rx,
            lyrics_tx,
            lyrics_rx,
            asr_tx.clone(),
            asr_rx,
            asr_child.clone(),
            asr_active.clone(),
        )
    } {
        Some(a) => a,
        None => std::process::exit(1),
    };

    if layout.asr != 0 {
        spawn_asr(
            &layout.asr_python,
            &layout.asr_script,
            &layout.asr_model,
            asr_tx,
            asr_child.clone(),
            asr_active,
        );
    }

    app.draw_edit();
    app.ensure_on_top();

    if start_edit {
        app.edit_mode = true;
        app.targetw = app.measure_target();
        app.curw = app.targetw;
        app.curh = app.pillh;
        app.anim = 0.0;
        app.place_window();
        app.show();
        app.draw();
    }

    let conn = match Connection::session() {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("gabes-island: no session bus: {e}");
            None
        }
    };

    let mut last_poll = now_ms().saturating_sub(1000);
    let mut last_tick = 0u128;
    let mut last_fs = 0u128;
    let mut last_raise = 0u128;
    let mut fullscreen = false;
    let fd = unsafe { xlib::XConnectionNumber(app.dpy) };

    loop {
        unsafe {
            while xlib::XPending(app.dpy) > 0 {
                let mut ev: xlib::XEvent = std::mem::zeroed();
                xlib::XNextEvent(app.dpy, &mut ev);
                match ev.get_type() {
                    xlib::Expose => {
                        if ev.expose.count == 0 {
                            if ev.expose.window == app.win {
                                app.draw();
                            } else if ev.expose.window == app.editwin {
                                app.draw_edit();
                            } else if ev.expose.window == app.widgetwin && app.widget_active {
                                app.update_widget(now_ms());
                            } else if ev.expose.window == app.fallwin && app.fall_mapped {
                                app.update_fall(now_ms());
                            }
                        }
                    }
                    xlib::ButtonPress => {
                        if ev.button.window == app.editwin && ev.button.button == 1 {
                            let on = !app.edit_mode;
                            app.set_edit_mode(on);
                        } else if ev.button.window == app.win {
                            let b = ev.button.button;
                            let mx = ev.button.x as f64;
                            let my = ev.button.y as f64;

                            if b == 2 { /* middle click: edit mode on/off */
                                let on = !app.edit_mode;
                                app.set_edit_mode(on);
                            } else if app.edit_mode {
                                match b {
                                    1 => {
                                        let rects = app
                                            .button_rects(app.curw as f64, app.curh as f64);
                                        let er = rects[3];
                                        let on_edit_btn = mx >= er.0
                                            && mx <= er.0 + er.2
                                            && my >= er.1
                                            && my <= er.1 + er.3;
                                        if on_edit_btn {
                                            app.set_edit_mode(false);
                                        } else {
                                            let boxes =
                                                app.element_boxes(app.curw as f64, app.curh as f64);
                                            let mut sel = -1;
                                            for (i, b) in boxes.iter().enumerate() {
                                                if mx >= b.0
                                                    && mx <= b.0 + b.2
                                                    && my >= b.1
                                                    && my <= b.1 + b.3
                                                {
                                                    sel = i as i32;
                                                }
                                            }
                                            if sel >= 0 {
                                                app.edit_sel = sel;
                                                app.draw();
                                            } else {
                                                let w = app.curw as f64;
                                                let h = app.curh as f64;
                                                let edge = 8.0;
                                                let ring = mx >= 0.0
                                                    && mx <= w
                                                    && my >= 0.0
                                                    && my <= h
                                                    && (mx < edge
                                                        || mx > w - edge
                                                        || my < 5.0
                                                        || my > h - 5.0);
                                                app.edit_sel = if ring { 5 } else { -1 };
                                                app.dragging = !ring;
                                                if ring {
                                                    app.draw();
                                                } else {
                                                    app.drag_start = (
                                                        ev.button.x_root,
                                                        ev.button.y_root,
                                                        app.x_offset,
                                                        app.topmargin,
                                                    );
                                                    app.draw();
                                                }
                                            }
                                        }
                                    }
                                    4 | 5 => {
                                        let up = b == 4;
                                        let shift = ev.button.state & xlib::ShiftMask != 0;
                                        let ctrl = ev.button.state & xlib::ControlMask != 0;

                                        let alt = ev.button.state & xlib::Mod1Mask != 0;
                                        if ctrl {
                                            // Ctrl + wheel: animation duration in seconds
                                            app.anim_secs = (app.anim_secs
                                                + if up { 0.1 } else { -0.1 })
                                                .clamp(0.2, 5.0);
                                            app.draw();
                                        } else {
                                        let d = if up { 2.0 } else { -2.0 };
                                        if alt {
                                            if (0..=4).contains(&app.edit_sel) {
                                                let d = if up { 2.0 } else { -2.0 };
                                                match app.edit_sel {
                                                    0 => {
                                                        app.art_h = (app.art_h + d)
                                                            .clamp(18.0, 120.0);
                                                        let need = (app.art_h + 6.0) as i32;
                                                        app.pillh = app.pillh.max(need);
                                                    }
                                                    1 => {
                                                        app.text_h = (app.text_h + d)
                                                            .clamp(16.0, 90.0)
                                                    }
                                                    2 => {
                                                        app.time_h = (app.time_h + d)
                                                            .clamp(10.0, 70.0)
                                                    }
                                                    3 => {
                                                        app.spec_h = (app.spec_h + d)
                                                            .clamp(10.0, 90.0)
                                                    }
                                                    _ => {
                                                        app.btn_h = (app.btn_h + d)
                                                            .clamp(10.0, 48.0)
                                                    }
                                                }
                                            } else {
                                                let min_h = app.min_pill_height();
                                                app.pillh = (app.pillh
                                                    + if up { 2 } else { -2 })
                                                    .clamp(min_h, 120);
                                            }
                                        } else if app.edit_sel == 5 || app.edit_sel < 0 {
                                            if shift {
                                                app.pad_x =
                                                    (app.pad_x + d * 2.0).clamp(0.0, 300.0);
                                            } else {
                                                let min_h =
                                                    (app.art_w + 6.0).max(30.0) as i32;
                                                app.pillh = (app.pillh
                                                    + if up { 2 } else { -2 })
                                                    .clamp(min_h, 120);
                                            }
                                        } else {
                                            match app.edit_sel {
                                                0 => {
                                                    app.art_w =
                                                        (app.art_w + d).clamp(18.0, 96.0);
                                                    let need = (app.art_w + 6.0) as i32;
                                                    app.pillh = app.pillh.max(need);
                                                }
                                                1 => {
                                                    app.extra_width = (app.extra_width + d * 3.0)
                                                        .clamp(0.0, 600.0);
                                                }
                                                2 => {
                                                    app.time_w = (app.time_w + d).clamp(12.0, 140.0)
                                                }
                                                3 => {
                                                    app.spec_w = (app.spec_w + d).clamp(10.0, 140.0)
                                                }
                                                4 => {
                                                    app.btn_w = (app.btn_w + d).clamp(10.0, 36.0)
                                                }
                                                _ => {}
                                            }
                                        }
                                        app.pillh = app.pillh.max(app.min_pill_height());
                                        app.targetw = app.measure_target();
                                        app.curw = app.targetw;
                                        app.curh = app.pillh;
                                        app.place_window();
                                        app.draw();
                                        }
                                    }
                                    _ => {}
                                }
                            } else {
                                let rects = app.button_rects(app.curw as f64, app.curh as f64);
                                let mut hit: Option<usize> = None;
                                for (i, r) in rects.iter().enumerate().take(4) {
                                    if mx >= r.0 && mx <= r.0 + r.2 && my >= r.1 && my <= r.1 + r.3 {
                                        hit = Some(i);
                                    }
                                }
                                if hit == Some(3) {
                                    let on = !app.edit_mode;
                                    app.set_edit_mode(on);
                                } else {
                                    let method = match hit {
                                        Some(0) => Some("Previous"),
                                        Some(1) => Some("PlayPause"),
                                        Some(2) => Some("Next"),
                                        _ if b == 1 => Some("PlayPause"),
                                        _ if b == 3 => Some("Next"),
                                        _ => None,
                                    };
                                    if let (Some(c), Some(m)) = (&conn, method) {
                                        mpris_action(c, &app.player, m);
                                    }
                                }
                            }
                        }
                    }
                    xlib::MotionNotify => {
                        if ev.motion.window == app.editwin {
                            if !app.edit_hover {
                                app.edit_hover = true;
                                app.draw_edit();
                            }
                        } else if ev.motion.window == app.win {
                            if app.edit_mode && app.dragging {
                                let dx = ev.motion.x_root - app.drag_start.0;
                                let dy = ev.motion.y_root - app.drag_start.1;
                                app.x_offset = app.drag_start.2 + dx;
                                app.topmargin = app.drag_start.3 + dy;
                                let lim = (app.sw - app.curw) / 2;
                                app.x_offset = app.x_offset.clamp(-lim, lim);
                                app.topmargin = app.topmargin.clamp(0, app.sh - app.curh);
                                app.place_window();
                                app.draw();
                            } else if !app.edit_mode {
                                let mx = ev.motion.x as f64;
                                let my = ev.motion.y as f64;
                                let rects =
                                    app.button_rects(app.curw as f64, app.curh as f64);
                                let mut hov = -1;
                                for (i, r) in rects.iter().enumerate().take(4) {
                                    if mx >= r.0 && mx <= r.0 + r.2 && my >= r.1 && my <= r.1 + r.3 {
                                        hov = i as i32;
                                    }
                                }
                                if hov != app.hover_btn {
                                    app.hover_btn = hov;
                                    app.draw();
                                }
                            }
                        }
                    }
                    xlib::ButtonRelease => {
                        if ev.button.window == app.win
                            && ev.button.button == 1
                            && app.edit_mode
                            && app.dragging
                        {
                            app.dragging = false;
                            app.save_config();
                        }
                    }
                    xlib::LeaveNotify => {
                        if ev.crossing.window == app.editwin && app.edit_hover {
                            app.edit_hover = false;
                            app.draw_edit();
                        } else if ev.crossing.window == app.win
                            && !app.edit_mode
                            && app.hover_btn != -1
                        {
                            app.hover_btn = -1;
                            app.draw();
                        }
                    }
                    xlib::MapNotify => {
                        if app.mapped && ev.map.window != app.win {
                            xlib::XRaiseWindow(app.dpy, app.win);
                        }
                    }
                    xlib::ConfigureNotify => {
                        if ev.configure.window == app.root {
                            let screen = xlib::XDefaultScreen(app.dpy);
                            app.sw = xlib::XDisplayWidth(app.dpy, screen);
                            app.sh = xlib::XDisplayHeight(app.dpy, screen);
                            app.place_window();
                            xlib::XMoveWindow(app.dpy, app.editwin, app.sw / 2 - 13, 12);
                            app.draw_edit();
                        }
                    }
                    _ => {}
                }
            }
        }

        // pick up fetched song lyrics
        while let Ok((key, text)) = app.lyrics_rx.try_recv() {
            if key == app.lyrics_req {
                let timed = parse_lrc(&text);
                if app.widget_sync && !timed.is_empty() {
                    app.lyrics_words = timed.iter().map(|(_, w)| w.clone()).collect();
                    app.lyrics_timed = timed;
                    app.lyrics_synced = true;
                } else {
                    app.lyrics_words = text
                        .split_whitespace()
                        .map(|w| {
                            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
                                .to_string()
                        })
                        .filter(|w| w.chars().count() > 1)
                        .collect();
                    app.lyrics_timed.clear();
                    app.lyrics_synced = false;
                }
                if app.widget_bpm > 0 && !app.lyrics_words.is_empty() {
                    let start = app.lyrics_timed.first().map(|x| x.0).unwrap_or(0);
                    let interval = (60_000u64 / app.widget_bpm / 2).max(120) as i64;
                    let words = app.lyrics_words.clone();
                    app.lyrics_timed = words
                        .iter()
                        .enumerate()
                        .map(|(i, w)| (start + i as i64 * interval, w.clone()))
                        .collect();
                    app.lyrics_synced = true;
                }
                app.lyrics_raw = app.lyrics_timed.clone();
                app.load_alignment(&key);
                app.sync_offset_ms = 0;
                app.drift_ema = 0.0;
                app.drift_n = 0;
                app.last_pos_ms = 0;
                app.recognized.clear();
                app.schedule_cursor = 0;
                app.last_word_idx = -1;
                app.plain_idx = 0;
            }
        }

        // pick up cover art that finished downloading
        while let Ok((url, path)) = app.art_rx.try_recv() {
            if url == app.loaded_art {
                if let Some(surf) = load_art_file(&path) {
                    app.art = Some(surf);
                }
            }
        }

        let t = now_ms();

        if t.saturating_sub(last_poll) >= 1000 {
            if let Some(c) = &conn {
                if let Some(fresh) = poll_media(c) {
                    let changed = fresh.title != app.media.title
                        || fresh.artist != app.media.artist
                        || fresh.art != app.media.art;
                    if fresh.art != app.loaded_art {
                        if let Some(path) = file_path_from_url(&fresh.art) {
                            if let Some(surf) = load_art_file(&path) {
                                app.art = Some(surf);
                            } /* else keep the previous cover */
                        } else if fresh.art.starts_with("http") {
                            let tx = app.art_tx.clone();
                            let url = fresh.art.clone();
                            thread::spawn(move || {
                                if let Some(path) = download_art(&url) {
                                    let _ = tx.send((url, path));
                                }
                            });
                        }
                        app.loaded_art = fresh.art.clone();
                    }
                    if changed {
                        app.media = fresh.clone();
                        app.player = fresh.player.clone();
                        app.pos_base_us = fresh.pos_us;
                        app.pos_base_ms = t;
                        app.targetw = app.measure_target();
                    } else {
                        app.media.playing = fresh.playing;
                        app.media.length_us = fresh.length_us;
                        app.pos_base_us = fresh.pos_us;
                        app.pos_base_ms = t;
                        app.player = fresh.player.clone();
                    }
                    if app.loaded_icon != app.player {
                        app.icon = load_player_icon(c, &app.player);
                        app.loaded_icon = app.player.clone();
                    }
                    if app.widget_source == 1 {
                        let key = format!("{}|{}", app.media.artist, app.media.title);
                        if !app.media.title.is_empty() && app.lyrics_req != key {
                            app.lyrics_req = key.clone();
                            app.lyrics_words.clear();
                            app.lyrics_timed.clear();
                            app.lyrics_synced = false;
                            app.last_word_idx = -1;
                            app.plain_idx = 0;
                            app.fall_last_idx = -1;
                            app.fall_words.clear();
                            app.fall_next = 0;
                            app.schedule_cursor = 0;
                            app.aligned.clear();
                            app.align_path.clear();
                            app.display_ready = app.warmup_ms == 0;
                            let tx = app.lyrics_tx.clone();
                            let artist = app.media.artist.clone();
                            let title = app.media.title.clone();
                            thread::spawn(move || fetch_lyrics(artist, title, tx));
                        }
                    }
                    app.have_media = true;
                } else {
                    app.have_media = false;
                }
            }
            last_poll = t;
        }

        if t.saturating_sub(last_fs) >= 500 {
            fullscreen = app.active_fullscreen();
            last_fs = t;
        }
        app.update_widget(t);
        app.update_fall(t);

        app.asr_active
            .store(app.asr_enabled && app.media.playing == 1, Ordering::Relaxed);
        if app.media.playing != 1 {
            if let Ok(mut slot) = app.asr_child.lock() {
                if let Some(mut child) = slot.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        while let Ok(line) = app.asr_rx.try_recv() {
            if let Some((_at, word)) = line.split_once('\t') {
                let word = word.trim();
                if word.is_empty() {
                    continue;
                }
                let pos = app.current_pos_ms();
                app.recognized.push((pos, word.to_string()));
                if let Some(dir) = cache_dir() {
                    let _ = fs::create_dir_all(&dir);
                    let _ = fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(dir.join("recognized.txt"))
                        .and_then(|mut f| {
                            f.write_all(format!("{}\t{}\n", mmss(pos), word).as_bytes())
                        });
                }
                if app.recognized.len() > 512 {
                    app.recognized.remove(0);
                }
                app.handle_asr_word(word);
            }
        }
        // keep above other windows without needless restacking
        if app.mapped && t.saturating_sub(last_raise) >= 800 {
            app.ensure_on_top();
            last_raise = t;
        }
        let want_show = app.edit_mode || (app.have_media && !fullscreen && app.media.playing == 1);
        if !want_show {
            app.bar_levels = [0.0; MAX_BARS];
            if let Ok(mut g) = app.bands.lock() {
                *g = [0.0; MAX_BARS];
            }
        }
        app.audio_active
            .store(app.mapped && app.media.playing == 1, Ordering::Relaxed);
        if want_show {
            if !app.mapped || app.dir < 0 {
                if !app.mapped {
                    app.curw = 48;
                    app.curh = 7;
                    app.anim = 0.0;
                    app.place_window();
                }
                app.show();
            }
        } else if app.mapped && app.dir >= 0 {
            app.hide();
        }

        if app.animating && t.saturating_sub(last_tick) >= 16 {
            let dt = (t.saturating_sub(last_tick) as f64 / 1000.0).clamp(0.001, 0.05);
            app.tick(dt);
            last_tick = t;
        } else if app.mapped && !app.animating && app.media.playing == 1 && t.saturating_sub(last_tick) >= 50 {
            app.draw();
            last_tick = t;
        }

        unsafe {
            let mut rfds: libc::fd_set = std::mem::zeroed();
            libc::FD_ZERO(&mut rfds);
            libc::FD_SET(fd, &mut rfds);
            let mut tv = libc::timeval {
                tv_sec: 0,
                tv_usec: 30_000,
            };
            libc::select(
                fd + 1,
                &mut rfds,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut tv,
            );
        }
    }
}
