//! Lyrics and cover-art fetching.
//!
//! Network access goes through the platform's own `curl` (Windows 10 1803+
//! ships `curl.exe`, macOS ships `/usr/bin/curl`), exactly like the Linux app.
//! That keeps the binary small and avoids a TLS stack in the process.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use island_core::config::cache_dir;
use island_core::lyrics::{clean_track_title, parse_json_string, url_encode};
use tiny_skia::Pixmap;

pub fn urldecode(s: &str) -> String {
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

pub fn file_path_from_url(url: &str) -> Option<String> {
    url.strip_prefix("file://").map(urldecode)
}

fn fbv_hash(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn ensure_cache() -> Option<PathBuf> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn curl(args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("curl")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if out.status.success() {
        Some(out.stdout)
    } else {
        None
    }
}

/// Downloads remote cover art into the cache and returns the local path.
pub fn download_art(url: &str) -> Option<String> {
    let dir = ensure_cache()?;
    let path = dir.join(format!("art-{:016x}.img", fbv_hash(url)));
    if let Ok(m) = std::fs::metadata(&path) {
        if m.len() > 0 {
            return Some(path.to_string_lossy().into_owned());
        }
    }
    let out = dir.join(format!("art-{:016x}.tmp", fbv_hash(url)));
    let ok = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "6",
            "-o",
            out.to_string_lossy().as_ref(),
            url,
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|st| st.success())
        .unwrap_or(false);
    if ok
        && std::fs::metadata(&out)
            .map(|m| m.len() > 0)
            .unwrap_or(false)
    {
        std::fs::rename(&out, &path).ok()?;
        Some(path.to_string_lossy().into_owned())
    } else {
        let _ = std::fs::remove_file(&out);
        None
    }
}

/// Fetches lyrics text (LRC or plain) from LRCLIB, then lyrics.ovh.
///
/// Blocking; call from a worker thread. The Linux build uses the same
/// endpoints and the same tried artist/title variants.
pub fn fetch_lyrics(artist: &str, title: &str) -> Option<String> {
    if artist.is_empty() || title.is_empty() {
        return None;
    }
    let mut artists: Vec<String> = vec![artist.to_string()];
    if let Some(first) = artist.split(',').next() {
        let first = first.trim().to_string();
        if first != artist {
            artists.push(first);
        }
    }
    let mut titles: Vec<String> = vec![title.to_string()];
    let clean = clean_track_title(title);
    if clean != title {
        titles.push(clean);
    }

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
            if let Some(body) = curl(&[
                "-fsSL",
                "--max-time",
                "6",
                "-H",
                "User-Agent: gabes-island/0.2",
                &url,
            ]) {
                let body = String::from_utf8_lossy(&body);
                if let Some(ly) = parse_json_string(&body, "syncedLyrics") {
                    return Some(ly);
                }
                if let Some(ly) = parse_json_string(&body, "plainLyrics") {
                    return Some(ly);
                }
            }
        }
    }

    let url = format!(
        "https://api.lyrics.ovh/v1/{}/{}",
        url_encode(artist),
        url_encode(title)
    );
    let body = curl(&["-fsSL", "--max-time", "6", &url])?;
    parse_json_string(&String::from_utf8_lossy(&body), "lyrics")
}

/// Decodes a cover into an RGBA pixmap (PNG/JPEG/WebP/GIF).
pub fn load_cover(path: &Path) -> Option<Pixmap> {
    let img = image::open(path).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    Pixmap::from_vec(img.into_raw(), tiny_skia::IntSize::from_wh(w, h)?)
}

/// Lyrics text cached on disk so a replay starts synced and offline works.
pub fn cache_lyrics(artist: &str, title: &str, text: &str) {
    let Some(dir) = ensure_cache() else { return };
    let path = dir.join(format!(
        "lyrics-{:016x}.lrc",
        fbv_hash(&format!("{artist}|{title}"))
    ));
    let _ = std::fs::write(path, text);
}

pub fn cached_lyrics(artist: &str, title: &str) -> Option<String> {
    let dir = cache_dir();
    let path = dir.join(format!(
        "lyrics-{:016x}.lrc",
        fbv_hash(&format!("{artist}|{title}"))
    ));
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urldecoding_and_file_urls() {
        assert_eq!(urldecode("a%20b%2Fc"), "a b/c");
        assert_eq!(
            file_path_from_url("file:///tmp/a%20b.mp3"),
            Some("/tmp/a b.mp3".into())
        );
        assert_eq!(file_path_from_url("https://x/y"), None);
    }

    #[test]
    fn hashes_are_stable() {
        assert_eq!(fbv_hash("abc"), fbv_hash("abc"));
        assert_ne!(fbv_hash("abc"), fbv_hash("abd"));
    }

    #[test]
    fn missing_cover_is_none() {
        assert!(load_cover(Path::new("/nonexistent/cover.png")).is_none());
    }
}
