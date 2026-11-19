//! The `~/.config/gabes/island.conf` schema.
//!
//! Key names, defaults and value spellings match the Linux app exactly, so a
//! single config file drives both builds.  One key is new: `capture_apps`
//! restricts which players produce a pill (default `spotify`).

use std::path::PathBuf;

// Pill metrics shared with the Linux implementation.
pub const PILL_MINW: i32 = 240;
pub const PILL_MAXW: i32 = 1000;
pub const PAD: f64 = 8.0;
pub const DEFAULT_ART_W: f64 = 32.0;
pub const DEFAULT_TIME_W: f64 = 30.0;
pub const DEFAULT_SPEC_W: f64 = 26.0;
pub const DEFAULT_BTN_W: f64 = 18.0;
pub const DEFAULT_BTN_GAP: f64 = 6.0;
pub const DEFAULT_EXTRA_W: f64 = 60.0;
pub const MAX_BARS: usize = 32;

/// How the random word widgets pick their text.
pub const SOURCE_WORDS: i32 = 0;
pub const SOURCE_LYRICS: i32 = 1;

/// Where the random word widgets appear.
pub const PLACE_RANDOM: i32 = 0;
pub const PLACE_ISLAND: i32 = 1;
pub const PLACE_BOTH: i32 = 2;

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub top: i32,
    pub height: i32,
    pub xoff: i32,
    pub extra: f64,
    pub art: f64,
    pub time: f64,
    pub spec: f64,
    pub btn: f64,
    pub gap: f64,
    pub anim: f64,
    pub padx: f64,
    pub art_h: f64,
    pub text_h: f64,
    pub time_h: f64,
    pub spec_h: f64,
    pub btn_h: f64,
    pub widgets: i32,
    pub wmin: u64,
    pub wmax: u64,
    pub wsource: i32,
    pub wplace: i32,
    pub w3d: i32,
    pub wcont: i32,
    pub wms: u64,
    pub wsync: i32,
    pub wfall: i32,
    pub wtrail: i32,
    pub wbpm: u64,
    pub wanti: i32,
    pub wdrift: u64,
    /// Accepted for config compatibility; ASR is Linux-only.
    pub asr: i32,
    pub warmup: u64,
    pub latency: i64,
    pub asr_python: String,
    pub asr_script: String,
    pub asr_model: String,
    /// Comma separated allowlist of players. Empty = every player.
    pub capture_apps: String,
    /// Messenger call banner (1 = on).
    pub calls: i32,
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
            wsource: SOURCE_LYRICS,
            wplace: PLACE_BOTH,
            w3d: 1,
            wcont: 1,
            wms: 500,
            wsync: 1,
            wfall: 1,
            wtrail: 1,
            wbpm: 0,
            wanti: 1,
            wdrift: 120,
            asr: 0,
            warmup: 0,
            latency: -1,
            asr_python: "~/.local/share/gabes-island/venv/bin/python".into(),
            asr_script: "~/.local/share/gabes-island/asr_vosk.py".into(),
            asr_model: "~/.local/share/gabes-island/vosk-model-small-en-us-0.15".into(),
            capture_apps: "spotify".into(),
            calls: 1,
        }
    }
}

impl Layout {
    /// Parses an INI-ish config body on top of the defaults.
    ///
    /// Unknown keys and malformed values are ignored, exactly like the Linux
    /// parser (a typo never stops the island from starting).
    pub fn parse(text: &str) -> Layout {
        let mut l = Layout::default();
        l.apply(text);
        l
    }

    pub fn apply(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim();
            match k.trim() {
                "top" => self.top = v.parse().unwrap_or(self.top),
                "height" => self.height = v.parse().unwrap_or(self.height),
                "x_offset" => self.xoff = v.parse().unwrap_or(self.xoff),
                "extra_width" => self.extra = v.parse().unwrap_or(self.extra),
                "art_w" => self.art = v.parse().unwrap_or(self.art),
                "time_w" => self.time = v.parse().unwrap_or(self.time),
                "spec_w" => self.spec = v.parse().unwrap_or(self.spec),
                "btn_w" => self.btn = v.parse().unwrap_or(self.btn),
                "btn_gap" => self.gap = v.parse().unwrap_or(self.gap),
                "anim_secs" => self.anim = v.parse().unwrap_or(self.anim),
                "pad_x" => self.padx = v.parse().unwrap_or(self.padx),
                "art_h" => self.art_h = v.parse().unwrap_or(self.art_h),
                "text_h" => self.text_h = v.parse().unwrap_or(self.text_h),
                "time_h" => self.time_h = v.parse().unwrap_or(self.time_h),
                "spec_h" => self.spec_h = v.parse().unwrap_or(self.spec_h),
                "btn_h" => self.btn_h = v.parse().unwrap_or(self.btn_h),
                "widgets" => self.widgets = v.parse().unwrap_or(self.widgets),
                "widget_min" => self.wmin = v.parse().unwrap_or(self.wmin),
                "widget_max" => self.wmax = v.parse().unwrap_or(self.wmax),
                "widget_source" => {
                    self.wsource = match v {
                        "lyrics" => SOURCE_LYRICS,
                        "words" => SOURCE_WORDS,
                        _ => v.parse().unwrap_or(self.wsource),
                    }
                }
                "widget_place" => {
                    self.wplace = match v {
                        "random" => PLACE_RANDOM,
                        "island" => PLACE_ISLAND,
                        "both" => PLACE_BOTH,
                        _ => v.parse().unwrap_or(self.wplace),
                    }
                }
                "widget_3d" => self.w3d = v.parse().unwrap_or(self.w3d),
                "widget_continuous" => self.wcont = v.parse().unwrap_or(self.wcont),
                "widget_word_ms" => self.wms = v.parse().unwrap_or(self.wms),
                "widget_sync" => self.wsync = v.parse().unwrap_or(self.wsync),
                "widget_waterfall" => self.wfall = v.parse().unwrap_or(self.wfall),
                "widget_trails" => self.wtrail = v.parse().unwrap_or(self.wtrail),
                "widget_bpm" => self.wbpm = v.parse().unwrap_or(self.wbpm),
                "widget_antidesync" => self.wanti = v.parse().unwrap_or(self.wanti),
                "widget_max_drift_ms" => self.wdrift = v.parse().unwrap_or(self.wdrift),
                "asr_enabled" => self.asr = v.parse().unwrap_or(self.asr),
                "widget_warmup_ms" => self.warmup = v.parse().unwrap_or(self.warmup),
                "widget_latency_ms" => self.latency = v.parse().unwrap_or(self.latency),
                "asr_python" => self.asr_python = v.to_string(),
                "asr_script" => self.asr_script = v.to_string(),
                "asr_model" => self.asr_model = v.to_string(),
                "capture_apps" => self.capture_apps = v.to_string(),
                "calls" => self.calls = v.parse().unwrap_or(self.calls),
                _ => {}
            }
        }
    }

    /// Serializes in the same order the Linux app writes, plus `capture_apps`.
    pub fn to_conf(&self) -> String {
        format!(
            "# Gabes Island layout\ntop={}\nheight={}\nx_offset={}\nextra_width={}\n\
             art_w={}\nart_h={}\ntext_h={}\ntime_w={}\ntime_h={}\nspec_w={}\nspec_h={}\n\
             btn_w={}\nbtn_h={}\nbtn_gap={}\nanim_secs={}\npad_x={}\nwidgets={}\n\
             widget_min={}\nwidget_max={}\nwidget_source={}\nwidget_place={}\n\
             widget_3d={}\nwidget_continuous={}\nwidget_word_ms={}\nwidget_sync={}\n\
             widget_waterfall={}\nwidget_trails={}\nwidget_bpm={}\nwidget_antidesync={}\n\
             widget_max_drift_ms={}\ncalls={}\nasr_enabled={}\nasr_python={}\nasr_script={}\n\
             asr_model={}\nwidget_warmup_ms={}\nwidget_latency_ms={}\ncapture_apps={}\n",
            self.top,
            self.height,
            self.xoff,
            self.extra,
            self.art,
            self.art_h,
            self.text_h,
            self.time,
            self.time_h,
            self.spec,
            self.spec_h,
            self.btn,
            self.btn_h,
            self.gap,
            self.anim,
            self.padx,
            self.widgets,
            self.wmin,
            self.wmax,
            self.wsource,
            self.wplace,
            self.w3d,
            self.wcont,
            self.wms,
            self.wsync,
            self.wfall,
            self.wtrail,
            self.wbpm,
            self.wanti,
            self.wdrift,
            self.calls,
            self.asr,
            self.asr_python,
            self.asr_script,
            self.asr_model,
            self.warmup,
            self.latency,
            self.capture_apps,
        )
    }

    /// Album art edge, clamped to the sizes the layout can draw.
    pub fn art_size(&self) -> f64 {
        self.art.clamp(18.0, 96.0)
    }

    /// Smallest pill height that still fits the tallest element.
    pub fn min_pill_height(&self) -> i32 {
        let tallest = self
            .art_h
            .max(self.text_h)
            .max(self.time_h)
            .max(self.spec_h)
            .max(self.btn_h);
        ((tallest + 6.0) as i32).max(30)
    }

    pub fn capture_app_list(&self) -> Vec<String> {
        crate::media::parse_capture_apps(&self.capture_apps)
    }
}

/// `~/.config/gabes` (Windows: `%APPDATA%\gabes`,
/// macOS: `~/Library/Application Support/gabes`).
///
/// `GABES_CONFIG_DIR` overrides the location on every platform — useful for
/// portable installs and for tests.
pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("GABES_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    platform_dir(PlatformDir::Config).join("gabes")
}

pub fn config_path() -> PathBuf {
    config_dir().join("island.conf")
}

pub fn words_path() -> PathBuf {
    config_dir().join("words.txt")
}

/// Cache for downloaded covers and fetched lyrics.
pub fn cache_dir() -> PathBuf {
    platform_dir(PlatformDir::Cache).join("gabes-island")
}

#[derive(Copy, Clone)]
enum PlatformDir {
    Config,
    Cache,
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
}

fn home() -> Option<PathBuf> {
    env_path("HOME").or_else(|| env_path("USERPROFILE"))
}

fn platform_dir(kind: PlatformDir) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let key = match kind {
            PlatformDir::Config => "APPDATA",
            PlatformDir::Cache => "LOCALAPPDATA",
        };
        if let Some(base) = env_path(key) {
            return match kind {
                PlatformDir::Config => base,
                PlatformDir::Cache => base.join("cache"),
            };
        }
        if let Some(home) = home() {
            return match kind {
                PlatformDir::Config => home.join(".config"),
                PlatformDir::Cache => home.join("AppData").join("Local").join("cache"),
            };
        }
        PathBuf::new()
    }
    #[cfg(target_os = "macos")]
    {
        let home = home().unwrap_or_default();
        match kind {
            PlatformDir::Config => home.join("Library").join("Application Support"),
            PlatformDir::Cache => home.join("Library").join("Caches"),
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        match kind {
            PlatformDir::Config => env_path("XDG_CONFIG_HOME")
                .or_else(|| home().map(|h| h.join(".config")))
                .unwrap_or_default(),
            PlatformDir::Cache => env_path("XDG_CACHE_HOME")
                .or_else(|| home().map(|h| h.join(".cache")))
                .unwrap_or_default(),
        }
    }
}

/// Reads the config file, falling back to defaults when it is missing.
pub fn load_layout() -> Layout {
    load_layout_from(&config_path())
}

pub fn load_layout_from(path: &std::path::Path) -> Layout {
    match std::fs::read_to_string(path) {
        Ok(text) => Layout::parse(&text),
        Err(_) => Layout::default(),
    }
}

pub fn save_layout(path: &std::path::Path, layout: &Layout) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, layout.to_conf())
}

/// Words for `widget_source=words`; the Linux defaults when the file is absent.
pub fn load_words(path: &std::path::Path) -> Vec<String> {
    let mut words = Vec::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                words.push(line.to_string());
            }
        }
    }
    if words.is_empty() {
        words = DEFAULT_WORDS.iter().map(|s| s.to_string()).collect();
    }
    words
}

pub const DEFAULT_WORDS: [&str; 11] = [
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
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_linux() {
        let l = Layout::default();
        assert_eq!((l.top, l.height, l.xoff), (10, 43, 0));
        assert_eq!(l.extra, 60.0);
        assert_eq!(l.art, 32.0);
        assert_eq!(l.wms, 500);
        assert_eq!(l.latency, -1);
        assert_eq!(l.wsource, SOURCE_LYRICS);
        assert_eq!(l.wplace, PLACE_BOTH);
        assert_eq!(l.capture_apps, "spotify");
        assert_eq!(l.min_pill_height(), 38);
    }

    #[test]
    fn parses_every_key_and_named_values() {
        let cfg = "\
# comment
top=24
height=50
x_offset=-4
extra_width=80
art_w=40
art_h=36
text_h=28
time_w=44
time_h=16
spec_w=40
spec_h=30
btn_w=20
btn_h=20
btn_gap=8
anim_secs=2.2
pad_x=6
widgets=0
widget_min=22
widget_max=50
widget_source=words
widget_place=island
widget_3d=0
widget_continuous=0
widget_word_ms=700
widget_sync=0
widget_waterfall=0
widget_trails=0
widget_bpm=128
widget_antidesync=0
widget_max_drift_ms=200
asr_enabled=1
widget_warmup_ms=10000
widget_latency_ms=180
capture_apps=spotify, com.apple.Music
";
        let l = Layout::parse(cfg);
        assert_eq!(l.top, 24);
        assert_eq!(l.art, 40.0);
        assert_eq!(l.art_h, 36.0);
        assert_eq!(l.wsource, SOURCE_WORDS);
        assert_eq!(l.wplace, PLACE_ISLAND);
        assert_eq!(l.w3d, 0);
        assert_eq!(l.wbpm, 128);
        assert_eq!(l.warmup, 10_000);
        assert_eq!(l.latency, 180);
        assert_eq!(
            l.capture_app_list(),
            vec!["spotify".to_string(), "com.apple.music".to_string()]
        );
    }

    #[test]
    fn malformed_values_keep_defaults() {
        let l = Layout::parse("top=abc\nheight=\nunknown=1\nnonsense\n");
        assert_eq!(l.top, 10);
        assert_eq!(l.height, 43);
    }

    #[test]
    fn numeric_widget_enums_still_accepted() {
        let l = Layout::parse("widget_source=0\nwidget_place=2\n");
        assert_eq!(l.wsource, SOURCE_WORDS);
        assert_eq!(l.wplace, PLACE_BOTH);
    }

    #[test]
    fn config_roundtrip() {
        let mut l = Layout::default();
        l.top = 33;
        l.spec = 41.0;
        l.capture_apps = "spotify".into();
        let again = Layout::parse(&l.to_conf());
        assert_eq!(l, again);
    }

    #[test]
    fn words_fallback_and_filtering() {
        let dir = std::env::temp_dir().join(format!("island-words-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("words.txt");
        std::fs::write(&file, "# c\nhej\n\n  dwa  \n").unwrap();
        assert_eq!(load_words(&file), vec!["hej", "dwa"]);
        std::fs::remove_file(&file).unwrap();
        assert_eq!(load_words(&file), DEFAULT_WORDS.to_vec());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
