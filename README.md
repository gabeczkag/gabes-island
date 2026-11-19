# gabes-island

A lightweight, highly customizable "Dynamic Island" for Linux/X11 with a
Cinnamon-friendly workflow: now playing, album art, real audio spectrum,
transport buttons, an edit mode with per-element boxes, falling lyrics and
a terminal-quality static-render cache.

> Written in Rust, no external status daemon, no compositor fork.

## Features

- MPRIS "now playing" pill (Spotify, Firefox, Rhythmbox, …)
- Album art with original proportions, player icon fallback
- Real spectrum (up to 32 log bands from the sink monitor)
- Transport buttons: previous / play-pause / next
- Edit mode with draggable element boxes:
  art, text, time, spectrum, buttons, whole bar
- Per-element width **and height** (wheel / Alt+wheel)
- Animation duration in seconds (Ctrl+wheel) and BPM grid
- Random lyric word widgets and a **falling-words waterfall** with trails
- Sync timeline kept in memory (LRC + optional beat grid, anti-drift cursor)
- CJK-aware fonts (Noto Sans CJK JP/KR) — no more tofu boxes
- Static render cache: only spectrum/time/progress regions are redrawn
- Everything persisted in `~/.config/gabes/island.conf`

> [!WARNING]
> **The Windows and macOS port is experimental: not tested, not recommended.**
> It compiles on both targets and the shared logic has a real test suite, but
> it has never been run on Windows or macOS hardware. Expect bugs, missing
> polish and platform behaviour that differs from the Linux app. Do not use it
> for daily listening yet; use the Linux/X11 build.

## Platforms

- **Linux / X11**: the root crate (`gabes-island`), built on X11, XShape,
  MPRIS/D-Bus and `/proc`. Tested on Linux Mint 22.3 with Cinnamon/Muffin.
  This is the supported build.
- **Windows 10/11**: experimental preview in `native/`
  (`gabes-island-native`) — **not tested, not recommended**. Layered
  always-on-top windows, SMTC now-playing and transport, WASAPI loopback
  spectrum; compile-checked and built in CI, never run on real hardware.
- **macOS 13+**: experimental preview in the same crate — **not tested, not
  recommended**. Floating `NSWindow`, MediaRemote (with an AppleScript
  fallback for Spotify/Music); the spectrum is a documented stub.

Linux binaries are attached to GitHub Releases. Native artifacts are
published with an `experimental-untested` suffix so they are not mistaken for
supported builds. See [docs/native-port.md](docs/native-port.md) for the
architecture, the exact parity matrix and the remaining stubs.

## Build (Linux/X11)

```sh
cargo build --release
```

System packages: `libcairo2-dev`, `libx11-dev`, `libxext-dev`,
`libfreetype-dev`, `libfontconfig-dev`.

## Build (Windows / macOS) — experimental, not tested, not recommended

> These steps build the native port. It has not been verified on real
> Windows or macOS machines; treat any result as a preview.

The repository is a Cargo workspace: the root package is still the Linux app,
so `cargo build --release` at the top level builds exactly what it always did.
The port is a separate crate:

```sh
cargo build --release -p gabes-island-native
```

No system libraries are needed — the renderer is `tiny-skia` + `cosmic-text`
and the Windows/macOS system APIs come through the `windows` and `objc2`
crates. The only runtime expectation is a `curl` binary for lyrics and remote
cover art (shipped with Windows 10 1803+ and with macOS).

Run it:

```sh
./target/release/gabes-island-native            # real system media
./target/release/gabes-island-native --mock     # fake Spotify session
./target/release/gabes-island-native --edit     # start in edit mode
./target/release/gabes-island-native --preview ./out    # PNGs, no window
./target/release/gabes-island-native --bench 3          # headless benchmark
./target/release/gabes-island-native --diagnose         # fonts + backends
```

The native binary is unsigned: on macOS, Gatekeeper may require
`xattr -d com.apple.quarantine` or a right-click → Open on first launch; on
Windows, SmartScreen shows "More info → Run anyway". Signing/notarisation is
a TODO, not a code problem.

## Run

```sh
./target/release/gabes-island
```

Optional autostart: copy the binary somewhere in `$PATH` and add a
`~/.config/autostart/gabes-island.desktop`, or use the project's
`scripts/install-island.sh` from the parent repository.

## Configuration

`~/.config/gabes/island.conf` (all keys optional):

```ini
top=10
height=43
x_offset=0
extra_width=60
art_w=32
art_h=32
text_h=26
time_w=30
time_h=14
spec_w=26
spec_h=28
btn_w=18
btn_h=18
btn_gap=6
anim_secs=1.5
pad_x=0
widgets=1
widget_min=20
widget_max=45
widget_source=lyrics
widget_place=both
widget_3d=1
widget_continuous=1
widget_word_ms=500
widget_sync=1
widget_waterfall=1
widget_trails=1
widget_bpm=0
widget_antidesync=1
widget_max_drift_ms=120
```

Words for `widget_source=words` live in `~/.config/gabes/words.txt`
(one per line). With `widget_source=lyrics`, lyrics are cached from LRCLIB
using the Spotify/MPRIS metadata; plain lyrics fall back to sequential words.

### Native port config

The native build reads the same keys from a platform path:

| Platform | Config |
| --- | --- |
| Linux | `~/.config/gabes/island.conf` (root crate) |
| Windows | `%APPDATA%\gabes\island.conf` |
| macOS | `~/Library/Application Support/gabes/island.conf` |

`GABES_CONFIG_DIR` overrides the directory on every platform.

One extra key exists for the native build:

```ini
capture_apps=spotify
```

A comma-separated, case-insensitive allowlist of players matched against the
stable app id (`com.spotify.client`, `Spotify.exe!Spotify`, …) and the display
name. The default is **Spotify only**. Use `capture_apps=spotify,com.apple.Music`
for more players, or `capture_apps=` (empty) to capture whatever plays.
`asr_*` keys are accepted but unused on Windows/macOS (ASR is Linux-only).

## Edit mode

- Mini sliders button at the top (when the pill is hidden) or the 4th
  button inside the pill, or middle click.
- Click a box to select it.
- Wheel = width, Alt+wheel = height, Shift+wheel = whole-bar width,
  Ctrl+wheel = animation seconds.
- Drag empty space to move the bar; middle click / sliders button to save.

## Synchronization

- Synced lyrics come from LRCLIB; word-level `<mm:ss.xx>` tags are used when
  present, otherwise each line's words are distributed by length (weighted),
  which tracks singing pace better than equal slots.
- Player position is polled every 500 ms while playing and interpolated with
  a monotonic clock between polls.
- Wake-ups are adaptive: 6 ms around a scheduled word, 40 ms otherwise, so
  releases land precisely without busy-looping.
- **Audio-output latency compensation:** `widget_latency_ms=-1` detects a
  Bluetooth sink and applies 200 ms (plain analog/digital outputs use 0).
  Words are released by the delayed clock, so they match what you hear.
  Set an explicit value to fine-tune.
- The in-memory timeline cursor releases every word exactly once; seeking
  rewinds or catches up without drift. `widget_bpm` can replace LRC timing
  with a beat grid (`30000/BPM` ms per word).
- No warm-up by default (`widget_warmup_ms=0`). With ASR enabled you can set
  10000 to start after ten seconds of listening.

## Live ASR alignment (optional, off by default)

At the start of a track the island starts a Vosk recognizer (English small
model, 1-second buffers). It does **not** pre-process the whole song: the
music plays normally, the recognizer listens in a separate process, and the
spoken words are compared with the word being displayed.

- The display starts after the first ~10 seconds (`widget_warmup_ms=10000`).
- Matched words resync the in-memory timeline; if the cursor is behind it may
  release a few words in one burst — by design.
- Directions are stored as **minutes:seconds**, e.g.:

  ```
  ~/.cache/gabes-island/recognized.txt
  2:56    i
  2:56    like
  3:00    right
  ~/.cache/gabes-island/align-<hash>.txt
  286     3:00    might
  ```

- Alignments are loaded on replay, so a previously listened track starts
  already in sync.
- `asr_enabled=0` is the default (lowest CPU/RAM). The LRC timeline,
  weighted word timing, latency compensation and the in-memory cursor all
  work without it.
- To enable it, set `asr_enabled=1` and point `asr_model` at a Vosk model.
  ASR cost here: **~12% CPU** and **~195 MB RSS**, only while playing.

## Fonts

Latin text uses Noto Sans / DejaVu Sans. Text containing Japanese, Chinese
or Korean characters automatically switches to **Noto Sans CJK JP/KR**, so
titles and lyric words no longer render as tofu boxes.

## Measured usage (this machine)

6-core laptop, 7.1 GB RAM, Linux Mint 22.3, X11, Cinnamon.

| State | App CPU | App RSS | App PSS | Xorg | Cinnamon |
| --- | --- | --- | --- | --- | --- |
| idle (paused) | 0.0% | 11.6 MB | 4.6 MB | — | — |
| playing + waterfall (before bar cache) | 6.6% | 15.9 MB | 8.7 MB | 21.0% | 11.6% |
| playing + waterfall, ASR off (current) | 3.0% | 15.2 MB | — | 7.6% | — |
| playing + waterfall + ASR | 6.8% | 15.4 MB | — | 8.8% | 10.4% |
| Vosk ASR process (optional) | 11.7% | — | — | — | — |

The bar keeps a cached static layer (pill, art, text, buttons) and per
frame only redraws three small regions: spectrum, time and progress.
The waterfall renders into a 560×760 column instead of a fullscreen overlay.

### Native port (Linux measurement, same machine)

The native crate is measured with its Linux/X11 development presenter, which
runs the identical state machine and renderer:

| State | App CPU | App RSS |
| --- | --- | --- |
| release build, window shown, mock player, waterfall off | 1.0% | 10.5 MB |
| headless `--bench 3`, pill only (render cost) | 0.27% of one core | — |
| headless `--bench 3`, pill + falling lyrics (render cost) | 0.51% of one core | 7.1 MB |

`--bench` reports work per simulated second at the app's own wake cadence
(50 ms while playing), so "0.27% of one core" is the renderer's share; the
windowed number also includes the event loop and the compositor. Release
binary: 4.0 MB. The same trick as on Linux keeps it cheap: the pill's static
layer is cached and only spectrum, time and progress are repainted, the
waterfall is its own small window, and an idle overlay wakes four times per
second instead of sixty.

## License

MIT — see `LICENSE`.
