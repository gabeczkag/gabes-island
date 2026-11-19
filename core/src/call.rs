//! Messenger call detection model.
//!
//! Browsers expose no API that lets another process answer a call, so the
//! island's job here is *detection and display*: figure out that a Messenger
//! conversation is on a call, who/what it is with, whether the camera is on,
//! and how long it has been running. The parts that differ per platform
//! (X11 window titles + PulseAudio + `/dev/video`, Windows UI Automation,
//! …) live in the platform crates; the parsing and the session state machine
//! are pure and tested here.
//!
//! Where a client does expose automation (the Messenger desktop app on
//! Windows/macOS), a backend can mark the call `controllable` and the island
//! shows Accept/Decline; the web client is informational only.

/// Voice or video.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallKind {
    Voice,
    Video,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallState {
    /// Ringing, not answered yet (only detectable from an "incoming" title).
    Incoming,
    /// The call is up (audio capture stream / answered call window).
    Active,
}

/// What a backend observed on one poll.
#[derive(Clone, Debug, PartialEq)]
pub struct CallEvidence {
    /// Conversation or caller name, already cleaned up.
    pub peer: String,
    pub state: CallState,
    pub kind: CallKind,
    /// True when the backend can actually accept/decline (desktop app).
    pub controllable: bool,
    /// Backend id, e.g. `messenger-web` or `messenger-desktop`.
    pub source: String,
}

/// A call that survived the session's hysteresis.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub peer: String,
    pub state: CallState,
    pub kind: CallKind,
    pub controllable: bool,
    pub source: String,
    /// First time this call was seen, in the caller's monotonic milliseconds.
    pub started_ms: u128,
}

impl Call {
    pub fn duration_ms(&self, now_ms: u128) -> i64 {
        now_ms.saturating_sub(self.started_ms) as i64
    }

    pub fn has_video(&self) -> bool {
        self.kind == CallKind::Video
    }
}

/// Debounces polling so a dropped `pactl` response or a brief title change
/// does not make the banner flicker.
#[derive(Clone, Debug)]
pub struct CallSession {
    current: Option<Call>,
    last_seen_ms: u128,
    /// How long the call survives without evidence.
    pub hangover_ms: u128,
}

impl Default for CallSession {
    fn default() -> Self {
        CallSession {
            current: None,
            last_seen_ms: 0,
            hangover_ms: 2_500,
        }
    }
}

impl CallSession {
    pub fn new() -> CallSession {
        CallSession::default()
    }

    /// Feeds one poll. Returns the call to display, if any.
    pub fn observe(&mut self, evidence: Option<CallEvidence>, now_ms: u128) -> Option<Call> {
        match evidence {
            Some(ev) => {
                let same = self
                    .current
                    .as_ref()
                    .map(|c| c.source == ev.source && c.peer == ev.peer)
                    .unwrap_or(false);
                if same {
                    if let Some(c) = &mut self.current {
                        c.state = ev.state;
                        c.kind = ev.kind;
                        c.controllable = ev.controllable;
                    }
                } else {
                    self.current = Some(Call {
                        peer: ev.peer,
                        state: ev.state,
                        kind: ev.kind,
                        controllable: ev.controllable,
                        source: ev.source,
                        started_ms: now_ms,
                    });
                }
                self.last_seen_ms = now_ms;
            }
            None => {
                if self.current.is_some()
                    && now_ms.saturating_sub(self.last_seen_ms) > self.hangover_ms
                {
                    self.current = None;
                }
            }
        }
        self.current.clone()
    }

    pub fn current(&self) -> Option<&Call> {
        self.current.as_ref()
    }

    pub fn clear(&mut self) {
        self.current = None;
    }
}

/// What a window title says about Messenger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessengerTitle {
    /// Text before the ` | Messenger` / ` - Messenger` suffix.
    pub conversation: String,
    pub call: bool,
    pub incoming: bool,
    pub video: bool,
}

/// Lowercase + strip Polish diacritics so "Połączenie" matches "polaczenie".
fn fold(s: &str) -> String {
    s.chars()
        .map(|c| match c.to_ascii_lowercase() {
            'ą' => 'a',
            'ć' => 'c',
            'ę' => 'e',
            'ł' => 'l',
            'ń' => 'n',
            'ó' => 'o',
            'ś' => 's',
            'ź' | 'ż' => 'z',
            other => other,
        })
        .collect()
}

const CALL_WORDS: [&str; 8] = [
    "rozmowa",
    "rozmow",
    "polaczenie",
    "dzwoni",
    "call",
    "ringing",
    "incoming",
    "meeting",
];

const INCOMING_WORDS: [&str; 5] = ["przychodz", "incoming", "dzwoni", "ringing", "nieodebran"];

const VIDEO_WORDS: [&str; 7] = [
    "video",
    "wideo",
    "kamera",
    "camera",
    "wideorozmowa",
    "videochat",
    "videocall",
];

fn has_any(haystack: &str, words: &[&str]) -> bool {
    words.iter().any(|w| haystack.contains(w))
}

/// True when this window title belongs to Messenger.
pub fn looks_like_messenger(title: &str) -> bool {
    fold(title).contains("messenger")
}

/// Splits `Conversation | Messenger` into its parts and classifies the call.
///
/// Returns `None` for windows that are not Messenger at all.
pub fn parse_messenger_title(title: &str) -> Option<MessengerTitle> {
    if !looks_like_messenger(title) {
        return None;
    }
    let conversation = title
        .rsplit_once(" | Messenger")
        .or_else(|| title.rsplit_once(" - Messenger"))
        .or_else(|| title.rsplit_once(" – Messenger"))
        .or_else(|| title.rsplit_once(" — Messenger"))
        .or_else(|| title.rsplit_once(" • Messenger"))
        .map(|(left, _)| left.trim().to_string())
        .unwrap_or_default();
    let folded = fold(&conversation);
    let call = has_any(&folded, &CALL_WORDS);
    Some(MessengerTitle {
        conversation,
        call,
        incoming: call && has_any(&folded, &INCOMING_WORDS),
        video: call && has_any(&folded, &VIDEO_WORDS),
    })
}

/// Strips a leading "Rozmowa z / Call with / Połączenie z".
pub fn strip_call_prefix(name: &str) -> String {
    let lower = fold(name);
    for prefix in [
        "rozmowa z ",
        "rozmowie z ",
        "polaczenie z ",
        "wideorozmowa z ",
        "video call with ",
        "call with ",
        "incoming call from ",
    ] {
        if lower.starts_with(prefix) {
            return name[prefix.len()..].trim().to_string();
        }
    }
    name.trim().to_string()
}

/// What a PulseAudio capture stream's `media.name` tells us.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MediaHint {
    pub call: bool,
    pub video: bool,
}

/// Names that are transport artefacts rather than a conversation.
const GENERIC_MEDIA: [&str; 8] = [
    "audiocallbackdriver",
    "webrtc",
    "audio capture",
    "audio stream",
    "firefox",
    "chrome",
    "chromium",
    "unknown",
];

/// Classifies a browser capture stream (PulseAudio `media.name`, e.g.
/// "Rozmowa grupowa").
pub fn classify_media_name(name: &str) -> MediaHint {
    let folded = fold(name);
    if folded.trim().is_empty() || GENERIC_MEDIA.iter().any(|g| folded == *g) {
        return MediaHint::default();
    }
    let call = has_any(&folded, &CALL_WORDS) || name.chars().any(|c| c.is_alphanumeric());
    MediaHint {
        call,
        video: has_any(&folded, &VIDEO_WORDS),
    }
}

/// A usable display label from a capture stream name, or `None` when the name
/// is a transport artefact.
pub fn peer_from_media_name(name: &str) -> Option<String> {
    let folded = fold(name);
    if folded.trim().is_empty() || GENERIC_MEDIA.iter().any(|g| folded == *g) {
        return None;
    }
    let cleaned = strip_call_prefix(name);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Merges title/audio/camera evidence into one call view.
///
/// `title`: the best Messenger window title, if one exists.
/// `media`: a browser capture stream name, if its `media.name` names a call.
/// `camera`: the browser currently holds a `/dev/video*` device.
pub fn compose(
    title: Option<&str>,
    media: Option<&str>,
    camera: bool,
    controllable: bool,
    source: &str,
) -> Option<CallEvidence> {
    let parsed = title.and_then(parse_messenger_title);
    let hint = media.map(classify_media_name).unwrap_or_default();
    let media_peer = media.and_then(peer_from_media_name);

    // A Messenger window must exist; audio from another browser tab (Meet,
    // WhatsApp) must not light the island up on its own.
    let parsed = parsed?;
    if !(hint.call || parsed.call) {
        return None;
    }

    let peer = media_peer
        .or_else(|| {
            let c = strip_call_prefix(&parsed.conversation);
            if c.is_empty() || fold(&c) == "messenger" {
                None
            } else {
                Some(c)
            }
        })
        .unwrap_or_else(|| "Messenger".to_string());

    let kind = if camera || hint.video || parsed.video {
        CallKind::Video
    } else {
        CallKind::Voice
    };
    let state = if parsed.incoming && !hint.call {
        CallState::Incoming
    } else {
        CallState::Active
    };

    Some(CallEvidence {
        peer,
        state,
        kind,
        controllable,
        source: source.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_messenger_titles_and_suffixes() {
        assert!(looks_like_messenger("BEZ KLARY | Messenger"));
        assert!(looks_like_messenger("Anna - Messenger"));
        assert!(!looks_like_messenger("GitHub - Mozilla Firefox"));

        let t = parse_messenger_title("BEZ KLARY | Messenger").unwrap();
        assert_eq!(t.conversation, "BEZ KLARY");
        assert!(!t.call);

        let t = parse_messenger_title("Rozmowa grupowa | Messenger").unwrap();
        assert_eq!(t.conversation, "Rozmowa grupowa");
        assert!(t.call);
        assert!(!t.video);
    }

    #[test]
    fn polish_call_words_match_with_diacritics() {
        let t = parse_messenger_title("Połączenie z Anna | Messenger").unwrap();
        assert!(t.call);
        assert!(!t.incoming);
        let t = parse_messenger_title("Połączenie przychodzące | Messenger").unwrap();
        assert!(t.call && t.incoming);
        let t = parse_messenger_title("Wideorozmowa | Messenger").unwrap();
        assert!(t.call && t.video);
    }

    #[test]
    fn english_titles_work() {
        let t = parse_messenger_title("Incoming video call | Messenger").unwrap();
        assert!(t.call && t.incoming && t.video);
        assert_eq!(strip_call_prefix("Incoming call from Anna"), "Anna");
        assert_eq!(strip_call_prefix("Call with Anna"), "Anna");
    }

    #[test]
    fn prefixes_are_stripped() {
        assert_eq!(strip_call_prefix("Rozmowa z Anną"), "Anną");
        assert_eq!(strip_call_prefix("Anna"), "Anna");
        assert_eq!(strip_call_prefix("  "), "");
    }

    #[test]
    fn media_names_are_classified() {
        assert_eq!(
            classify_media_name("Rozmowa grupowa"),
            MediaHint {
                call: true,
                video: false
            }
        );
        assert!(classify_media_name("Video call with Anna").video);
        assert!(!classify_media_name("AudioCallbackDriver").call);
        assert_eq!(peer_from_media_name("Rozmowa grupowa"), Some("Rozmowa grupowa".into()));
        assert_eq!(peer_from_media_name("webrtc"), None);
    }

    #[test]
    fn compose_needs_a_messenger_window() {
        // audio alone (another browser tab) must not trigger
        assert!(compose(None, Some("Rozmowa grupowa"), true, false, "web").is_none());
        // non-messenger windows are ignored
        assert!(compose(Some("Meet - abc"), Some("Rozmowa"), false, false, "web").is_none());
    }

    #[test]
    fn compose_prefers_the_call_name_and_marks_video() {
        let ev = compose(
            Some("BEZ KLARY | Messenger"),
            Some("Rozmowa grupowa"),
            true,
            false,
            "messenger-web",
        )
        .unwrap();
        assert_eq!(ev.peer, "Rozmowa grupowa");
        assert_eq!(ev.kind, CallKind::Video);
        assert_eq!(ev.state, CallState::Active);

        // title-only incoming call, no audio yet
        let ev = compose(
            Some("Połączenie przychodzące | Messenger"),
            None,
            false,
            true,
            "messenger-desktop",
        )
        .unwrap();
        assert_eq!(ev.state, CallState::Incoming);
        assert!(ev.controllable);
        assert_eq!(ev.kind, CallKind::Voice);
    }

    #[test]
    fn compose_falls_back_to_the_title_conversation() {
        let ev = compose(Some("Anna Nowak | Messenger"), Some("Audio capture"), false, false, "web");
        // no call evidence at all
        assert!(ev.is_none());

        let ev = compose(Some("Rozmowa z Anna | Messenger"), None, false, false, "web").unwrap();
        assert_eq!(ev.peer, "Anna");
        assert_eq!(ev.state, CallState::Active);
    }

    #[test]
    fn session_holds_through_a_missed_poll() {
        let mut s = CallSession::new();
        let ev = Some(CallEvidence {
            peer: "Anna".into(),
            state: CallState::Active,
            kind: CallKind::Voice,
            controllable: false,
            source: "web".into(),
        });
        assert_eq!(s.observe(ev.clone(), 1_000).unwrap().peer, "Anna");
        // a dropped poll keeps the banner
        assert!(s.observe(None, 2_000).is_some());
        assert!(s.current().is_some());
        // long silence ends it
        assert!(s.observe(None, 10_000).is_none());
        assert!(s.current().is_none());
    }

    #[test]
    fn session_starts_a_new_call_when_the_peer_changes() {
        let mut s = CallSession::new();
        let make = |peer: &str| {
            Some(CallEvidence {
                peer: peer.into(),
                state: CallState::Active,
                kind: CallKind::Voice,
                controllable: false,
                source: "web".into(),
            })
        };
        s.observe(make("Anna"), 1_000);
        let c = s.observe(make("Bartek"), 5_000).unwrap();
        assert_eq!(c.peer, "Bartek");
        assert_eq!(c.started_ms, 5_000, "new call gets a new clock");
        assert_eq!(c.duration_ms(8_500), 3_500);
    }
}
