# Native port: Windows and macOS

> [!WARNING]
> **Status: experimental — not tested, not recommended.**
> Nothing in this crate has been run on Windows or macOS hardware. It
> type-checks against the real `windows`/`objc2` APIs, builds in CI on both
> platforms, and its shared logic and renderer are unit tested on Linux, but
> presentation, media integration and input have never been exercised on the
> target operating systems. Treat every `✓` below as *implemented and
> compile-checked*, not as *verified working*. The Linux/X11 build remains the
> supported app.

The Linux/X11 app in the repository root is unchanged and still builds with
`cargo build --release`. Windows and macOS are served by a new workspace
member, `gabes-island-native`, that shares the interesting logic with the
Linux app through a pure-Rust crate, `island-core`.

```
island/
├── Cargo.toml          workspace: root package (Linux) + members
├── src/main.rs         Linux/X11 app, untouched
├── core/               island-core  — no platform APIs, fully unit tested
└── native/             gabes-island-native — Windows/macOS overlay
    ├── src/app.rs          state machine (pill, widget, waterfall, edit mode)
    ├── src/render.rs       tiny-skia drawing of the three surfaces
    ├── src/paint.rs        Canvas: logical px → device px, shapes, text
    ├── src/text.rs         cosmic-text shaping + glyph sprite cache
    ├── src/media/          MediaSource: mock, Windows SMTC, macOS MediaRemote
    ├── src/spectrum/       SpectrumSource: mock, WASAPI loopback, macOS stub
    ├── src/platform/       winit glue + per-OS presenters
    ├── src/preview.rs      headless PNG renderer
    └── src/bench.rs        headless benchmark
```

`cargo build --release` at the root still builds only the Linux binary,
because a workspace with a root package defaults to that package. Use
`-p gabes-island-native` for the port.

## Why this shape

* **No Tauri/Electron.** The whole overlay is four borderless windows. A web
  view would cost 60–100 MB of RSS to draw a 400×43 pill.
* **Same four surfaces as X11.** The Linux app uses four X windows (pill,
  sliders button, word widget, waterfall); the native port opens four winit
  windows with the same geometry rules, so the layout code transfers 1:1.
* **Pure logic in `island-core`.** Config parsing, LRC parsing, weighted word
  timing, the lyric cursor, latency compensation, the drift guard, pill
  geometry, hit boxes, the spring animation and the waterfall physics have no
  platform dependencies and 54 unit tests. Both the Windows and macOS builds
  use that code; the Linux crate keeps its inline copies so release `v0.1.0`
  stays reproducible (a later, mechanical migration can drop them).
* **Renderer:** `tiny-skia` (premultiplied RGBA, SIMD) + `cosmic-text`
  (shaping + system font fallback). No GPU context, no shader compilation,
  no per-frame allocations beyond the surfaces themselves.

## Frame pipeline

```
MediaSource ─┐
             ├─► App::update()  ─► App::draw() ─► Overlay::present(surface, pixmap)
Spectrum ────┘        │                │
                   Timeline         static layer (cached)
                   Spring           + dynamic regions (spectrum/time/progress)
                   Fall physics     + waterfall / widget surfaces
```

* The pill's static layer (background, art, title, artist, buttons, edit
  boxes) is rendered only when its hash changes — track, layout, hover,
  animation value. Per frame the app copies that layer and repaints only the
  spectrum, the elapsed time and the progress bar.
* The loop asks the app for its next wake-up: 16 ms while the pill animates,
  33 ms with a word widget, 50 ms while playing (waterfall at 20 Hz), 250 ms
  when idle. A paused overlay costs four wake-ups per second.
* Every surface is drawn in logical pixels scaled by the window's DPI factor
  (`scale`), so the same config looks identical on a 96 DPI panel and a Retina
  display.

## Windows backend

| Concern | Implementation |
| --- | --- |
| Window | winit creates a borderless, always-on-top, non-activating window; `WS_EX_LAYERED \| WS_EX_NOACTIVATE \| WS_EX_TOOLWINDOW` are added, then each frame goes through `UpdateLayeredWindow` with a premultiplied BGRA DIB. |
| Transparency | True per-pixel alpha. Fully transparent pixels are click-through by design — no hit-test code. |
| Now playing | SMTC (`Windows.Media.Control`): sessions, media properties, timeline, playback info, thumbnail stream, transport commands. |
| Player filter | `capture_apps` matched against `SourceAppUserModelId` and a prettified app name. |
| Spectrum | WASAPI loopback of the default render endpoint on a worker thread, float32/PCM16 down-mixed to mono, fed to the shared Goertzel analyser. |
| Fullscreen | `SHQueryUserNotificationState` hides the island when a fullscreen app owns the foreground. |
| Latency | `widget_latency_ms=-1` resolves to 0 (no Bluetooth auto-detection on Windows — set it explicitly for Bluetooth headphones). |

Winit's own transparency handling is disabled on Windows: `SetLayeredWindowAttributes`
(which winit uses) and `UpdateLayeredWindow` are mutually exclusive, so the
port owns the layered window completely.

## macOS backend

| Concern | Implementation |
| --- | --- |
| Window | Borderless winit window, `setOpaque(false)`, clear background, no shadow, `NSFloatingWindowLevel`, joins all Spaces, stationary, `FullScreenAuxiliary`. |
| Presentation | The view is layer-backed; every frame becomes a `CGImage` assigned to `CALayer.contents` (premultiplied RGBA, `kCGImageAlphaPremultipliedLast`), with `contentsScale` set to the DPI factor. |
| Now playing | `MRMediaRemoteGetNowPlayingInfo` / `MRMediaRemoteGetNowPlayingApplicationPID` / `MRMediaRemoteSendCommand`, loaded with `dlopen` from the PrivateFrameworks directory. Title, artist, album, elapsed, duration, playback rate and artwork bytes come from the dictionary. |
| Player filter | The PID is mapped to a bundle identifier through `NSRunningApplication`, then matched with `capture_apps`. |
| Fallback | When MediaRemote is unavailable, blocked or has nothing for an allowed app: AppleScript against Spotify or Music (public, no entitlements), polled on the same 500 ms thread, also used for transport. |
| Spectrum | **Stub.** macOS has no WASAPI-style loopback. A Core Audio process tap (`AudioHardwareCreateProcessTap`, macOS 14.4+) or ScreenCaptureKit audio needs a user permission prompt and, for the tap, an audio-capture entitlement; neither can be verified in this environment, so the spectrum area shows the idle bars and everything else works. |
| Fullscreen | Not detected (no cheap public API); the island stays on top. |

### MediaRemote caveat

MediaRemote is a private framework. It has been stable for a decade, but Apple
has restricted it for non-entitled apps (notably around macOS 15.4). The
AppleScript fallback exists precisely for that case and covers the default
`capture_apps=spotify`; artwork is only available through MediaRemote, so with
the fallback the pill shows the note placeholder. The port never asks for
Accessibility or Screen Recording permission.

## Parity matrix

`✓` in the Windows/macOS columns means *implemented and compile-checked*.
Only the Linux column has been run on real hardware.

| Feature | Linux (root crate) | Windows (native) | macOS (native) |
| --- | --- | --- | --- |
| Now-playing pill, art, title, artist | ✓ | ✓ | ✓ |
| Sweep-in/out animation (spring) | ✓ | ✓ | ✓ |
| Real spectrum, 32 log bands | ✓ PulseAudio | ✓ WASAPI loopback | stub (idle bars) |
| Transport buttons | ✓ MPRIS | ✓ SMTC | ✓ MediaRemote / AppleScript |
| Progress line + elapsed time | ✓ | ✓ | ✓ |
| Edit mode: boxes, wheel/Alt/Shift/Ctrl, drag, saved config | ✓ | ✓ | ✓ |
| Random word widget (`widget_source=words/lyrics`) | ✓ | ✓ | ✓ |
| Falling-words waterfall + trails | ✓ | ✓ | ✓ |
| Lyrics from LRCLIB, plain fallback, disk cache | ✓ | ✓ | ✓ |
| Synced timeline, weighted words, word tags, BPM grid | ✓ | ✓ | ✓ |
| Latency compensation, anti-drift | ✓ | ✓ (manual value) | ✓ (manual value) |
| CJK font fallback | ✓ Noto CJK | ✓ Yu Gothic/Malgun/YaHei | ✓ Hiragino/PingFang/Apple SD Gothic |
| `capture_apps` allowlist | ignored | ✓ (default Spotify) | ✓ (default Spotify) |
| Hide over fullscreen apps | ✓ NetWM | ✓ | — |
| Live ASR alignment | ✓ optional | — (Linux-only) | — |
| Single binary release | ✓ | ✓ | ✓ |

## Testing

* `cargo test -p island-core` — 54 tests: config round-trip, LRC/word tags,
  weighted timing, seek/rewind/burst cursor, latency, drift guard, geometry,
  spring bounds, spectrum bands, waterfall physics, capture allowlist.
* `cargo test -p gabes-island-native` — 30 tests: renderer pixels (pill,
  CJK, edit boxes, widget, waterfall), text layout/rotation, canvas scaling,
  app state machine (show/hide, lyric sync, edit interactions, drag + save,
  transport), preview generation.
* `--preview DIR` renders seven PNGs headlessly; CI asserts they exist on all
  three runners, so a regression in the shared renderer fails the matrix.
* `--bench N` runs the state machine and renderer against a null overlay.
* CI cross-checks the Windows and macOS code with
  `cargo check --target x86_64-pc-windows-gnu` and `--target x86_64-apple-darwin`
  on every change (no linker required), and builds both natively.

## What is still missing

0. **Hardware verification.** Nothing in the native port has run on Windows or
   macOS yet; until it does, the port stays "not tested, not recommended".

1. **macOS spectrum** — Core Audio process tap or ScreenCaptureKit audio,
   behind a permission prompt.
2. **macOS fullscreen detection** and **Bluetooth latency auto-detection on
   both platforms**.
3. **Code signing / notarisation** and a `.app` bundle for macOS; an installer
   or portable zip for Windows.
4. **Linux crate migration** to `island-core` (deduplicate the shared logic) —
   deliberately deferred so `v0.1.0` stays byte-identical.
5. **Live ASR** remains Linux-only (Vosk sidecar is a Python process bound to
   PulseAudio in the current implementation).
