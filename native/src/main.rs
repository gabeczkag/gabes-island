//! **gabes-island-native** — the Windows/macOS overlay.
//!
//! Same config file, same look and the same feature set as the Linux/X11 app,
//! rendered with tiny-skia + cosmic-text into four layered windows.
//!
//! ```text
//! gabes-island-native                 # run the overlay
//! gabes-island-native --mock          # fake Spotify session, no system media API
//! gabes-island-native --edit          # start in edit mode
//! gabes-island-native --preview ./out # render PNG previews, no window
//! ```

mod app;
mod bench;
mod media;
mod net;
mod paint;
mod platform;
mod preview;
mod render;
mod spectrum;
mod text;

use std::path::PathBuf;
use std::process::ExitCode;

use island_core::config;

const HELP: &str = "\
gabes-island-native — now-playing island for Windows and macOS

STATUS: experimental preview. Not tested on real Windows/macOS hardware and
        not recommended for daily use. The Linux/X11 build is the supported app.

USAGE:
    gabes-island-native [OPTIONS]

OPTIONS:
    --mock                 use a fake Spotify session (no system media API)
    --edit                 start in edit mode (drag boxes, wheel = resize)
    --top N                override island top margin
    --height N             override pill height
    --extra-width N        override the gap after the text
    --x-offset N           override the horizontal offset
    --preview DIR          render every preview PNG into DIR and exit
    --screenshot FILE      render one desktop composite PNG and exit
    --diagnose             print resolved fonts and backend names, then exit
    --bench [SECONDS]      headless render benchmark (default 2 s), then exit
    -h, --help             show this help
    -V, --version          print the version

CONFIG:
    Linux    ~/.config/gabes/island.conf
    Windows  %APPDATA%\\gabes\\island.conf
    macOS    ~/Library/Application Support/gabes/island.conf
    Override the directory with GABES_CONFIG_DIR.

    Keys are shared with the Linux app; `capture_apps` (default `spotify`)
    restricts which players are shown, e.g. `capture_apps=spotify,com.apple.Music`
    or an empty value to capture every player.
";

fn main() -> ExitCode {
    let mut layout = config::load_layout();
    let mut mock = false;
    let mut start_edit = false;
    let mut preview_dir: Option<PathBuf> = None;
    let mut screenshot: Option<PathBuf> = None;
    let mut diagnose = false;
    let mut bench_secs: Option<u64> = None;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("gabes-island-native {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--mock" => {
                mock = true;
                i += 1;
            }
            "--edit" => {
                start_edit = true;
                i += 1;
            }
            "--diagnose" => {
                diagnose = true;
                i += 1;
            }
            "--bench" => {
                let secs = args
                    .get(i + 1)
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(2);
                bench_secs = Some(secs.clamp(1, 30));
                i += if args
                    .get(i + 1)
                    .map(|v| v.parse::<u64>().is_ok())
                    .unwrap_or(false)
                {
                    2
                } else {
                    1
                };
            }
            "--top" | "--height" | "--extra-width" | "--x-offset" => {
                let Some(v) = args.get(i + 1) else {
                    eprintln!("{HELP}");
                    return ExitCode::from(2);
                };
                match args[i].as_str() {
                    "--top" => layout.top = v.parse().unwrap_or(layout.top),
                    "--height" => layout.height = v.parse().unwrap_or(layout.height),
                    "--extra-width" => layout.extra = v.parse().unwrap_or(layout.extra),
                    _ => layout.xoff = v.parse().unwrap_or(layout.xoff),
                }
                i += 2;
            }
            "--preview" => {
                preview_dir = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--screenshot" => {
                screenshot = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            other => {
                eprintln!("gabes-island-native: unknown argument {other}\n");
                eprintln!("{HELP}");
                return ExitCode::from(2);
            }
        }
    }

    layout.height = layout.height.max(layout.min_pill_height());

    if diagnose {
        let mut engine = text::TextEngine::new();
        println!("{}", preview::font_report(&mut engine));
        let samples = ["Warszawa", "東京の夜", "서울", "naïve café"];
        for s in samples {
            let ok = preview::font_sample(&mut engine, s);
            println!("  {:<12} {}", s, if ok { "ok" } else { "NO GLYPHS" });
        }
        println!("media backend: {}", media::backend_name());
        println!("config: {}", config::config_path().display());
        return ExitCode::SUCCESS;
    }

    if let Some(secs) = bench_secs {
        // Headless: no window is created, nothing appears on screen.
        let lines = bench::run(secs);
        println!("headless render benchmark, {secs}s of simulated playback each");
        for l in &lines {
            println!(
                "  {:<26} wake {:>5.0} ms  {:>7.2} ms work per second  =>  {:>5.2}% of one core  ({} presents / {} ticks)",
                l.label,
                l.wake_ms,
                l.ms_per_second,
                l.cpu_percent(),
                l.paints,
                l.ticks
            );
        }
        if let Some(rss) = bench::rss_mib() {
            println!("  peak RSS (process): {rss:.1} MiB");
        }
        return ExitCode::SUCCESS;
    }

    if let Some(dir) = preview_dir {
        match preview::write_previews(&dir) {
            Ok(files) => {
                for f in files {
                    println!("{}", f.display());
                }
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("gabes-island-native: preview failed: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    if let Some(path) = screenshot {
        return match preview::screenshot(&path) {
            Ok(p) => {
                println!("{}", p.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("gabes-island-native: screenshot failed: {e}");
                ExitCode::FAILURE
            }
        };
    }

    eprintln!(
        "gabes-island-native: EXPERIMENTAL — not tested on Windows/macOS hardware, \
         not recommended for daily use (the Linux/X11 build is the supported app)"
    );
    match platform::run(platform::Options {
        mock,
        start_edit,
        layout,
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gabes-island-native: {e}");
            ExitCode::FAILURE
        }
    }
}
