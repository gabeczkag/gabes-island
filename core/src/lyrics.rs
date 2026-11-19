//! LRC parsing and the lyric timeline construction.
//!
//! Ported one-to-one from the Linux app so a track that syncs there syncs the
//! same way on Windows and macOS:
//!
//! 1. every `[mm:ss.xx]` line becomes a line start (multi-timestamps expand),
//! 2. word-level `<mm:ss.xx>` tags win when a line has them,
//! 3. otherwise the line's words are distributed by **weight**
//!    (`chars + 1.5`), which follows singing pace better than equal slots.

use crate::text::clean_word;

/// `[mm:ss.xx]` / `<mm:ss.xxx>` payload to milliseconds.
pub fn parse_lrc_time(tag: &str) -> Option<i64> {
    let mut parts = tag.split(':');
    let minutes: i64 = parts.next()?.trim().parse().ok()?;
    let seconds: f64 = parts.next()?.trim().parse().ok()?;
    if !seconds.is_finite() {
        return None;
    }
    Some(((minutes * 60) as f64 * 1000.0 + seconds * 1000.0) as i64)
}

/// `<mm:ss.xx>word<mm:ss.xx>word` tags inside one lyric line.
pub fn parse_word_tags(text: &str) -> Vec<(i64, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(lt) = rest.find('<') {
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else { break };
        let Some(t) = parse_lrc_time(&after[..gt]) else {
            rest = &after[gt + 1..];
            continue;
        };
        let content = &after[gt + 1..];
        let next = content.find('<').unwrap_or(content.len());
        for w in content[..next].split_whitespace() {
            let w = clean_word(w);
            if !w.is_empty() {
                out.push((t, w));
            }
        }
        rest = &content[next..];
    }
    out
}

/// Lays the words of one line across `[start, next)` by character weight.
pub fn weighted_word_times(start: i64, next: i64, words: &[String]) -> Vec<(i64, String)> {
    let dur = (next - start).max(200);
    let weights: Vec<f64> = words
        .iter()
        .map(|w| (w.chars().count() as f64).max(1.0) + 1.5)
        .collect();
    let total: f64 = weights.iter().sum::<f64>().max(0.001);
    let mut acc = 0.0;
    let mut out = Vec::with_capacity(words.len());
    for (i, w) in words.iter().enumerate() {
        let at = start + (dur as f64 * acc / total) as i64;
        out.push((at, w.clone()));
        acc += weights[i];
    }
    out
}

/// Turns LRC (or plain text) into a flat `(ms, word)` list.
pub fn parse_lrc(text: &str) -> Vec<(i64, String)> {
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
        let tagged = parse_word_tags(text);
        if !tagged.is_empty() {
            words.extend(tagged);
            continue;
        }
        words.extend(weighted_word_times(start, next, &ws));
    }
    words
}

/// Drops `[..]` tags and joins the remaining lines with single spaces.
pub fn strip_lrc(text: &str) -> String {
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

/// Plain lyrics -> words for the unsynced widget, minus punctuation.
pub fn plain_words(text: &str) -> Vec<String> {
    untagged_words(text)
        .into_iter()
        .filter(|w| w.chars().count() > 1)
        .collect()
}

/// Plain lyrics -> every word including one-letter ones.
pub fn untagged_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(clean_word)
        .filter(|w| !w.is_empty())
        .collect()
}

/// `Song (Remastered)` / `Song - Live` / `Song [feat. X]` -> `Song`.
pub fn clean_track_title(title: &str) -> String {
    let mut t = title.to_string();
    for sep in [" (", " [", " - ", " \u{2013} "] {
        if let Some(i) = t.find(sep) {
            t.truncate(i);
        }
    }
    t.trim().to_string()
}

/// Percent-encodes everything outside the unreserved set (query values).
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
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

/// Minimal extractor for one string field of a JSON body (LRCLIB replies).
///
/// Kept dependency-free: the Linux build has no JSON crate either.
pub fn parse_json_string(body: &str, key: &str) -> Option<String> {
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

/// Parsed lyric payload plus the derived timeline.
#[derive(Clone, Debug, Default)]
pub struct ParsedLyrics {
    /// One entry per word, lazily timed.
    pub timed: Vec<(i64, String)>,
    /// The same words without times (widget pool).
    pub words: Vec<String>,
    pub synced: bool,
}

impl ParsedLyrics {
    /// Applies the app's policy: keep timings only when `widget_sync` is on.
    /// `widget_bpm > 0` replaces LRC timing with a half-beat word grid.
    pub fn from_payload(text: &str, sync: bool, bpm: u64) -> ParsedLyrics {
        let timed = parse_lrc(text);
        let mut out = if sync && !timed.is_empty() {
            ParsedLyrics {
                words: timed.iter().map(|(_, w)| w.clone()).collect(),
                timed,
                synced: true,
            }
        } else {
            ParsedLyrics {
                words: plain_words(text),
                timed: Vec::new(),
                synced: false,
            }
        };
        if bpm > 0 && !out.words.is_empty() {
            let start = out.timed.first().map(|x| x.0).unwrap_or(0);
            out.timed = bpm_grid(&out.words, bpm, start);
            out.synced = true;
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }
}

/// One word every half beat (`30000/BPM` ms), never faster than 120 ms.
pub fn bpm_grid(words: &[String], bpm: u64, start_ms: i64) -> Vec<(i64, String)> {
    if bpm == 0 {
        return Vec::new();
    }
    let interval = (60_000u64 / bpm / 2).max(120) as i64;
    words
        .iter()
        .enumerate()
        .map(|(i, w)| (start_ms + i as i64 * interval, w.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_times() {
        assert_eq!(parse_lrc_time("00:12.34"), Some(12_340));
        assert_eq!(parse_lrc_time("1:05"), Some(65_000));
        assert_eq!(parse_lrc_time("bad"), None);
        assert_eq!(parse_lrc_time("0:nan"), None);
    }

    #[test]
    fn multi_timestamp_lines_expand() {
        let lrc = "[00:01.00][00:31.00]same line\n[00:10.00]other";
        let words = parse_lrc(lrc);
        let times: Vec<i64> = words.iter().map(|(t, _)| *t).collect();
        // 1s line has two words, then the 10s line, then the 31s repeat.
        assert_eq!(times[0], 1_000);
        assert_eq!(words.len(), 5);
        assert!(times.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(words[3].0, 31_000);
        assert_eq!(words[3].1, "same");
    }

    #[test]
    fn weighted_words_fill_the_line_interval() {
        let ws: Vec<String> = ["a", "longerword", "mid"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let out = weighted_word_times(1_000, 4_000, &ws);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].0, 1_000);
        assert!(out[1].0 > out[0].0);
        // a short word starts early, a long one pushes everything after it out
        let equal_split = 1_000 + 3_000 / 3;
        assert!(out[1].0 < equal_split);
        assert!(out[2].0 > equal_split);
        assert!(out[2].0 < 4_000);
        assert_eq!(out[2].1, "mid");
    }

    #[test]
    fn word_tags_override_weighting() {
        let lrc = "[00:10.00]<00:10.20>Hello <00:10.80>world";
        let words = parse_lrc(lrc);
        assert_eq!(words[0], (10_200, "Hello".to_string()));
        assert_eq!(words[1], (10_800, "world".to_string()));
    }

    #[test]
    fn lrc_skips_metadata_and_empty_text() {
        let words = parse_lrc("[ar:Artist]\n[00:05.00]\n[00:06.00]real (words)");
        assert_eq!(words[0], (6_000, "real".to_string()));
        assert_eq!(words[1].1, "words");
    }

    #[test]
    fn plain_payload_falls_back_to_words() {
        let p = ParsedLyrics::from_payload("Hello, world — again", true, 0);
        assert!(!p.synced);
        assert!(p.timed.is_empty());
        assert_eq!(p.words, vec!["Hello", "world", "again"]);
    }

    #[test]
    fn bpm_replaces_the_timeline() {
        let words: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let grid = bpm_grid(&words, 120, 500);
        // 60_000/120/2 = 250 ms
        assert_eq!(grid[0], (500, "a".into()));
        assert_eq!(grid[2].0, 1_000);
        assert!(bpm_grid(&words, 4000, 0)[1].0 >= 120);
        let p = ParsedLyrics::from_payload("[00:01.00]one two", true, 128);
        assert!(p.synced);
        assert_eq!(p.timed.len(), 2);
    }

    #[test]
    fn strip_and_clean_titles() {
        assert_eq!(strip_lrc("[00:01.00]a\n[00:02.00]b"), "a b");
        assert_eq!(clean_track_title("Song (Remastered 2011)"), "Song");
        assert_eq!(clean_track_title("Song - Live"), "Song");
        assert_eq!(clean_track_title("Song"), "Song");
    }

    #[test]
    fn json_extraction_handles_escapes() {
        let body = r#"{"syncedLyrics":"[00:01.00]aé\nb"}"#;
        assert_eq!(
            parse_json_string(body, "syncedLyrics").unwrap(),
            "[00:01.00]aé\nb"
        );
        assert_eq!(parse_json_string(r#"{"x":""}"#, "x"), None);
        assert_eq!(parse_json_string("{}", "x"), None);
    }

    #[test]
    fn url_encoding() {
        assert_eq!(url_encode("a b/c"), "a%20b%2Fc");
        assert_eq!(url_encode("Daft-Punk_x.y~"), "Daft-Punk_x.y~");
    }
}
