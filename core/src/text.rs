//! Text helpers that do not depend on a font library.
//!
//! `cosmic-text`/`cairo` both need a "which family has glyphs for this?"
//! decision before shaping; that decision lives here.

// CJK families differ per platform; the renderer resolves the first one that
// the font database actually has.
#[cfg(target_os = "windows")]
pub const CJK_JP: &str = "Yu Gothic";
#[cfg(target_os = "windows")]
pub const CJK_KR: &str = "Malgun Gothic";
#[cfg(target_os = "macos")]
pub const CJK_JP: &str = "Hiragino Sans";
#[cfg(target_os = "macos")]
pub const CJK_KR: &str = "Apple SD Gothic Neo";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub const CJK_JP: &str = "Noto Sans CJK JP";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub const CJK_KR: &str = "Noto Sans CJK KR";

/// Latin families, in preference order (Linux build, kept for parity).
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub const WIDGET_FONTS: [&str; 2] = ["Noto Sans", "DejaVu Sans"];
#[cfg(target_os = "windows")]
pub const WIDGET_FONTS: [&str; 2] = ["Segoe UI", "Tahoma"];
#[cfg(target_os = "macos")]
pub const WIDGET_FONTS: [&str; 2] = ["Helvetica Neue", "Arial"];

/// Ordered families to try, resolved against the font database at startup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontCandidates {
    pub latin: &'static [&'static str],
    /// Secondary Latin family used by the random word widgets.
    pub alt: &'static [&'static str],
    pub cjk_jp: &'static [&'static str],
    pub cjk_kr: &'static [&'static str],
}

pub fn font_candidates() -> FontCandidates {
    #[cfg(target_os = "windows")]
    {
        FontCandidates {
            latin: &["Segoe UI Variable Text", "Segoe UI", "Tahoma", "Arial"],
            alt: &["Tahoma", "Georgia", "Consolas"],
            cjk_jp: &[
                "Yu Gothic UI",
                "Yu Gothic",
                "Meiryo UI",
                "Meiryo",
                "Microsoft YaHei UI",
                "Microsoft YaHei",
                "MS Gothic",
            ],
            cjk_kr: &["Malgun Gothic", "Gulim", "Batang"],
        }
    }
    #[cfg(target_os = "macos")]
    {
        FontCandidates {
            latin: &["SF Pro Text", "Helvetica Neue", "Helvetica", "Arial"],
            alt: &["Arial", "Georgia", "Menlo"],
            cjk_jp: &[
                "Hiragino Sans",
                "Hiragino Kaku Gothic ProN",
                "PingFang SC",
                "Songti SC",
            ],
            cjk_kr: &["Apple SD Gothic Neo", "AppleGothic", "Nanum Gothic"],
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        FontCandidates {
            latin: &["Noto Sans", "DejaVu Sans", "Liberation Sans", "FreeSans"],
            alt: &["DejaVu Sans", "Liberation Sans", "FreeSans"],
            cjk_jp: &[
                "Noto Sans CJK JP",
                "Noto Sans JP",
                "Noto Sans CJK SC",
                "Source Han Sans JP",
            ],
            cjk_kr: &[
                "Noto Sans CJK KR",
                "Noto Sans KR",
                "Noto Sans CJK JP",
                "Source Han Sans KR",
            ],
        }
    }
}

pub fn has_hangul(text: &str) -> bool {
    text.chars().any(|c| {
        let u = c as u32;
        (0xAC00..=0xD7A3).contains(&u) || (0x1100..=0x11FF).contains(&u)
    })
}

pub fn has_cjk(text: &str) -> bool {
    text.chars().any(|c| {
        let u = c as u32;
        (0x3000..=0x303F).contains(&u)
            || (0x3040..=0x30FF).contains(&u)
            || (0x3400..=0x4DBF).contains(&u)
            || (0x4E00..=0x9FFF).contains(&u)
            || (0xF900..=0xFAFF).contains(&u)
            || (0xFF66..=0xFF9D).contains(&u)
    })
}

/// The family to shape `text` with, or `fallback` for Latin text.
pub fn font_for_text<'a>(text: &str, fallback: &'a str) -> &'a str {
    if has_hangul(text) {
        CJK_KR
    } else if has_cjk(text) {
        CJK_JP
    } else {
        fallback
    }
}

/// Families to try after `primary` when a glyph is still missing.
pub fn cjk_fallback_chain(text: &str) -> &'static [&'static str] {
    let c = font_candidates();
    if has_hangul(text) {
        c.cjk_kr
    } else if has_cjk(text) {
        c.cjk_jp
    } else {
        &[]
    }
}

/// `1:05` for microseconds, clamped at zero like the Linux app.
pub fn fmt_time(us: i64) -> String {
    let secs = us.max(0) / 1_000_000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// `1:05` for milliseconds.
pub fn mmss(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Parses `m:ss` (used by the ASR direction files).
pub fn parse_mmss(s: &str) -> Option<i64> {
    let (m, sec) = s.split_once(':')?;
    let m: i64 = m.trim().parse().ok()?;
    let sec: i64 = sec.trim().parse().ok()?;
    Some(m * 60_000 + sec * 1000)
}

/// Shortens `text` with an ellipsis until `measure` says it fits.
///
/// Binary search over char boundaries, mirroring the Linux implementation so
/// both builds cut titles at the same place.
pub fn ellipsize_with<F>(text: &str, maxw: f64, measure: F) -> String
where
    F: Fn(&str) -> f64,
{
    if text.is_empty() || measure(text) <= maxw {
        return text.to_string();
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let cut = if mid < chars.len() {
            chars[mid].0
        } else {
            text.len()
        };
        let candidate = format!("{}\u{2026}", &text[..cut]);
        if measure(&candidate) <= maxw {
            lo = mid;
        } else {
            hi = mid.saturating_sub(1);
        }
    }
    let cut = if lo < chars.len() {
        chars[lo].0
    } else {
        text.len()
    };
    format!("{}\u{2026}", &text[..cut])
}

/// Trims punctuation off a lyric word, keeping apostrophes (Linux parity).
pub fn clean_word(w: &str) -> String {
    w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_detection() {
        assert!(has_cjk("東京の夜"));
        assert!(!has_cjk("Warszawa"));
        assert!(has_hangul("서울"));
        assert!(!has_hangul("Tokyo"));
        assert_eq!(font_for_text("東京", "Noto Sans"), CJK_JP);
        assert_eq!(font_for_text("서울", "Noto Sans"), CJK_KR);
        assert_eq!(font_for_text("Warszawa", "Noto Sans"), "Noto Sans");
        assert!(cjk_fallback_chain("warszawa").is_empty());
    }

    #[test]
    fn time_formatting() {
        assert_eq!(fmt_time(0), "0:00");
        assert_eq!(fmt_time(65_000_000), "1:05");
        assert_eq!(fmt_time(-5), "0:00");
        assert_eq!(mmss(185_000), "3:05");
        assert_eq!(parse_mmss("3:05"), Some(185_000));
        assert_eq!(parse_mmss("x:05"), None);
    }

    #[test]
    fn ellipsizing_respects_width() {
        let measure = |s: &str| s.chars().count() as f64;
        assert_eq!(ellipsize_with("short", 10.0, measure), "short");
        let cut = ellipsize_with("abcdefghij", 5.0, measure);
        assert_eq!(cut, "abcd\u{2026}");
        assert!(measure(&cut) <= 5.0);
        let cjk = ellipsize_with("東京の夜の音楽", 4.0, measure);
        assert_eq!(cjk, "東京の\u{2026}");
    }

    #[test]
    fn clean_words_keep_apostrophes() {
        assert_eq!(clean_word("(hello)"), "hello");
        assert_eq!(clean_word("don't,"), "don't");
        assert_eq!(clean_word("—"), "");
    }
}
