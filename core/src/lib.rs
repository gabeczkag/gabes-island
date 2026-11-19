//! Platform-independent core of **gabes-island**.
//!
//! The Linux/X11 app in the repository root carries its own inline copies of
//! these routines (kept untouched so release `v0.1.0` stays bit-for-bit
//! reproducible).  The Windows/macOS port in `native/` is built on this crate,
//! which makes the interesting parts testable on any host:
//!
//! * [`config`] — every `island.conf` key, defaults and the platform config path
//! * [`lyrics`] — LRC parsing, word tags, weighted word timing, plain fallback
//! * [`timing`] — the in-memory lyric cursor, latency compensation, drift guard
//! * [`geom`] — pill geometry, hit boxes, dynamic regions, the spring animation
//! * [`fall`] — the falling-words waterfall physics
//! * [`media`] — the `Media` struct and the `capture_apps` allowlist
//! * [`spectrum`] — log band mapping and level smoothing
//! * [`text`] — CJK script detection, time formatting, ellipsizing

pub mod call;
pub mod config;
pub mod fall;
pub mod geom;
pub mod lyrics;
pub mod media;
pub mod rng;
pub mod spectrum;
pub mod text;
pub mod timing;

pub use call::{Call, CallEvidence, CallKind, CallSession, CallState};
pub use config::Layout;
pub use media::{Media, PlayerAction};
pub use rng::Rng;
