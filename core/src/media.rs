//! The current-track snapshot and the `capture_apps` allowlist.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Media {
    /// 1 playing, 2 paused, 0 stopped.  Same encoding as the Linux app.
    pub playing: u8,
    /// Stable player id: MPRIS bus name, Windows AUMID, macOS bundle id.
    pub player: String,
    /// Human readable app name ("Spotify").
    pub app_name: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Remote URL or local path to the cover.
    pub art: String,
    /// In-memory cover (Windows SMTC hands out thumbnails as a stream).
    pub cover: Option<Vec<u8>>,
    pub length_us: i64,
    pub pos_us: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerAction {
    Previous,
    PlayPause,
    Next,
}

impl Media {
    pub fn is_playing(&self) -> bool {
        self.playing == 1
    }

    /// Key used to decide whether lyrics/art must be reloaded.
    pub fn track_key(&self) -> String {
        format!("{}|{}", self.artist, self.title)
    }

    /// Key that tells the app whether the artwork must be reloaded.
    pub fn art_key(&self) -> String {
        if let Some(bytes) = &self.cover {
            return format!("mem:{}:{}", bytes.len(), simple_hash(bytes));
        }
        self.art.clone()
    }

    /// Prefer the album name over the artist when the artist is empty.
    pub fn display_artist(&self) -> &str {
        if self.artist.is_empty() {
            &self.album
        } else {
            &self.artist
        }
    }
}

/// Splits `capture_apps` into lowercase needles.
///
/// An empty value means "capture every player" (the Linux behaviour).
pub fn parse_capture_apps(cfg: &str) -> Vec<String> {
    cfg.split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Does this player pass the allowlist?
///
/// Matching is a case-insensitive substring test against the stable player id
/// and the human name, so `spotify` matches `com.spotify.client`,
/// `Spotify.exe!Spotify` (Windows AUMID) and `Spotify`.
pub fn is_capture_allowed(apps: &[String], player_id: &str, app_name: &str) -> bool {
    if apps.is_empty() {
        return true;
    }
    let id = player_id.to_lowercase();
    let name = app_name.to_lowercase();
    if id.is_empty() && name.is_empty() {
        return false;
    }
    apps.iter().any(|needle| {
        (!id.is_empty() && id.contains(needle.as_str()))
            || (!name.is_empty() && name.contains(needle.as_str()))
    })
}

fn simple_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_allowlist_captures_everything() {
        let apps = parse_capture_apps("");
        assert!(is_capture_allowed(
            &apps,
            "org.mpris.MediaPlayer2.firefox",
            "Firefox"
        ));
        let apps = parse_capture_apps("   ");
        assert!(is_capture_allowed(&apps, "whatever", "Whatever"));
    }

    #[test]
    fn spotify_default_matches_every_naming_scheme() {
        let apps = parse_capture_apps("spotify");
        assert!(is_capture_allowed(&apps, "com.spotify.client", "Spotify"));
        assert!(is_capture_allowed(&apps, "Spotify.exe", "Spotify"));
        assert!(is_capture_allowed(
            &apps,
            "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify",
            ""
        ));
        assert!(is_capture_allowed(
            &apps,
            "org.mpris.MediaPlayer2.spotify",
            ""
        ));
        // ...and excludes everything else
        assert!(!is_capture_allowed(
            &apps,
            "org.mpris.MediaPlayer2.firefox",
            "Firefox"
        ));
        assert!(!is_capture_allowed(&apps, "com.apple.Music", "Music"));
        assert!(!is_capture_allowed(&apps, "", ""));
    }

    #[test]
    fn multiple_entries_and_case_insensitive() {
        let apps = parse_capture_apps(" Spotify , COM.APPLE.Music ");
        assert!(is_capture_allowed(&apps, "com.apple.music", "Music"));
        assert!(is_capture_allowed(&apps, "", "Spotify Premium"));
        assert!(!is_capture_allowed(&apps, "vlc", "VLC"));
    }

    #[test]
    fn media_helpers() {
        let m = Media {
            artist: String::new(),
            album: "Album".into(),
            ..Media::default()
        };
        assert_eq!(m.display_artist(), "Album");
        assert_eq!(m.track_key(), "|");
        assert!(!m.is_playing());
        let m = Media {
            playing: 1,
            ..Media::default()
        };
        assert!(m.is_playing());
    }
}
