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

## Build

```sh
cargo build --release
```

System packages: `libcairo2-dev`, `libx11-dev`, `libxext-dev`,
`libfreetype-dev`, `libfontconfig-dev`.

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

## Edit mode

- Mini sliders button at the top (when the pill is hidden) or the 4th
  button inside the pill, or middle click.
- Click a box to select it.
- Wheel = width, Alt+wheel = height, Shift+wheel = whole-bar width,
  Ctrl+wheel = animation seconds.
- Drag empty space to move the bar; middle click / sliders button to save.

## Measured usage (this machine)

6-core laptop, 7.1 GB RAM, Linux Mint 22.3, X11, Cinnamon.

| State | App CPU | App RSS | App PSS | Xorg | Cinnamon |
| --- | --- | --- | --- | --- | --- |
| idle (paused) | 0.0% | 11.6 MB | 4.6 MB | — | — |
| playing + waterfall (before bar cache) | 6.6% | 15.9 MB | 8.7 MB | 21.0% | 11.6% |
| playing + waterfall (static cache + slim fall area) | 3.8% | 14.9 MB | 7.5 MB | 8.8% | 10.4% |

The bar keeps a cached static layer (pill, art, text, buttons) and per
frame only redraws three small regions: spectrum, time and progress.
The waterfall renders into a 560×760 column instead of a fullscreen overlay.

## License

MIT — see `LICENSE`.
