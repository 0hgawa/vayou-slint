//! Small shared helpers: ISO-639 language names and aspect-ratio matching.

/// Human-readable language name for an ISO-639-1/2 code (or the code itself if
/// unknown). Mirrors the WebView build's `lang-names.ts`.
pub fn lang_name(code: &str) -> String {
    if code.is_empty() {
        return String::new();
    }
    let c = code.to_ascii_lowercase().replace('_', "-");
    // Regional variants whose region changes the displayed name.
    let regional = match c.as_str() {
        "pt-br" | "pob" | "pb" => Some("Portuguese (BR)"),
        "pt-pt" => Some("Portuguese (PT)"),
        "es-419" | "es-la" | "es-mx" | "es-ar" | "es-co" => Some("Spanish (LA)"),
        "zh-cn" | "zh-hans" | "zh-sg" => Some("Chinese (Simplified)"),
        "zh-tw" | "zh-hk" | "zh-hant" => Some("Chinese (Traditional)"),
        _ => None,
    };
    if let Some(n) = regional {
        return n.to_string();
    }
    // Otherwise resolve the base code, ignoring any region (en-US -> en).
    let name = match c.split('-').next().unwrap_or(&c) {
        "eng" | "en" => "English",
        "por" | "pt" => "Portuguese",
        "spa" | "es" => "Spanish",
        "fre" | "fra" | "fr" => "French",
        "deu" | "ger" | "de" => "German",
        "ita" | "it" => "Italian",
        "jpn" | "ja" => "Japanese",
        "kor" | "ko" => "Korean",
        "zho" | "chi" | "zh" => "Chinese",
        "rus" | "ru" => "Russian",
        "ara" | "ar" => "Arabic",
        "hin" | "hi" => "Hindi",
        "tur" | "tr" => "Turkish",
        "pol" | "pl" => "Polish",
        "nld" | "dut" | "nl" => "Dutch",
        "swe" | "sv" => "Swedish",
        "nor" | "nob" | "no" | "nb" => "Norwegian",
        "dan" | "da" => "Danish",
        "fin" | "fi" => "Finnish",
        "ces" | "cze" | "cs" => "Czech",
        "slk" | "slo" | "sk" => "Slovak",
        "slv" | "sl" => "Slovenian",
        "hun" | "hu" => "Hungarian",
        "ron" | "rum" | "ro" => "Romanian",
        "bul" | "bg" => "Bulgarian",
        "hrv" | "hr" => "Croatian",
        "srp" | "scc" | "sr" => "Serbian",
        "ukr" | "uk" => "Ukrainian",
        "ell" | "gre" | "el" => "Greek",
        "heb" | "he" | "iw" => "Hebrew",
        "tha" | "th" => "Thai",
        "vie" | "vi" => "Vietnamese",
        "ind" | "id" => "Indonesian",
        "msa" | "may" | "ms" => "Malay",
        "fil" | "tl" => "Filipino",
        "cat" | "ca" => "Catalan",
        "eus" | "baq" | "eu" => "Basque",
        "glg" | "gl" => "Galician",
        "lit" | "lt" => "Lithuanian",
        "lav" | "lv" => "Latvian",
        "est" | "et" => "Estonian",
        "isl" | "ice" | "is" => "Icelandic",
        "kat" | "geo" | "ka" => "Georgian",
        "hye" | "arm" | "hy" => "Armenian",
        "aze" | "az" => "Azerbaijani",
        "kaz" | "kk" => "Kazakh",
        "fas" | "per" | "fa" => "Persian",
        "urd" | "ur" => "Urdu",
        "ben" | "bn" => "Bengali",
        "tam" | "ta" => "Tamil",
        "tel" | "te" => "Telugu",
        "lat" | "la" => "Latin",
        _ => return code.to_string(),
    };
    name.to_string()
}

/// The five cycle-aspect ratios + a few extras shown in the context menu,
/// matched against mpv's decimal `video-aspect-override` value. Returns the
/// canonical ratio string ("16:9", "-1", …) so the UI can highlight it.
pub fn match_aspect(current: &str) -> String {
    if current == "-1" || current.parse::<f64>().is_ok_and(|v| v <= 0.0) {
        return "-1".into();
    }
    let Ok(cur) = current.parse::<f64>() else { return "-1".into() };
    const RATIOS: &[(&str, f64)] = &[
        ("16:9", 16.0 / 9.0), ("4:3", 4.0 / 3.0), ("21:9", 21.0 / 9.0),
        ("16:10", 16.0 / 10.0), ("5:4", 5.0 / 4.0), ("1:1", 1.0),
        ("2.35:1", 2.35), ("2.39:1", 2.39),
    ];
    RATIOS.iter()
        .find(|(_, v)| (cur - v).abs() < 0.001)
        .map_or_else(|| "-1".into(), |(s, _)| (*s).to_string())
}

/// Parse a `#rrggbb` (or `#rgb`) hex colour into RGB bytes. Falls back to white.
pub fn hex_to_rgb(hex: &str) -> (u8, u8, u8) {
    let h = hex.trim_start_matches('#');
    let parse = |s: &str| u8::from_str_radix(s, 16).unwrap_or(255);
    match h.len() {
        6 => (parse(&h[0..2]), parse(&h[2..4]), parse(&h[4..6])),
        3 => (parse(&h[0..1].repeat(2)), parse(&h[1..2].repeat(2)), parse(&h[2..3].repeat(2))),
        _ => (255, 255, 255),
    }
}

/// Format RGB bytes as `#rrggbb`.
pub fn rgb_to_hex(r: u8, g: u8, b: u8) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// RGB (0–255) → HSV (`hue` 0–360, `sat`/`val` 0–1) to seed the in-app colour
/// picker from a stored colour. Mirrors Slint's `hsv()` so the round trip lands
/// on the same point.
pub fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if r >= g && r >= b {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if g >= b {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let s = if max == 0.0 { 0.0 } else { d / max };
    (h, s, max)
}

/// Human file size, mirroring MediaInfoPanel's `fmt`.
pub fn fmt_size(bytes: i64) -> String {
    let b = bytes as f64;
    if bytes < 1024 { format!("{bytes} B") }
    else if bytes < 1_048_576 { format!("{:.1} KB", b / 1024.0) }
    else if bytes < 1_073_741_824 { format!("{:.1} MB", b / 1_048_576.0) }
    else { format!("{:.2} GB", b / 1_073_741_824.0) }
}

/// Human bitrate, mirroring MediaInfoPanel's `fmtBitrate`.
pub fn fmt_bitrate(bps: i64) -> String {
    if bps < 1000 { format!("{bps} bps") }
    else if bps < 1_000_000 { format!("{} kbps", bps / 1000) }
    else { format!("{:.1} Mbps", bps as f64 / 1_000_000.0) }
}

/// Format an OpenSubtitles download count ("12345" → "12.3k dl").
pub fn fmt_downloads(count: &str) -> String {
    let Ok(n) = count.parse::<f64>() else { return count.to_string() };
    if n >= 1_000_000.0 { format!("{:.1}M dl", n / 1_000_000.0) }
    else if n >= 1000.0 { format!("{:.1}k dl", n / 1000.0) }
    else { format!("{n} dl") }
}

/// Human duration "1h 2m 3s" / "2m 3s".
pub fn fmt_duration(s: f64) -> String {
    let (h, m, sec) = ((s / 3600.0) as i64, ((s % 3600.0) / 60.0) as i64, (s % 60.0) as i64);
    if h > 0 { format!("{h}h {m}m {sec}s") } else { format!("{m}m {sec}s") }
}

/// Short, friendly format tag for a track codec (`subrip` -> "SRT",
/// `ac3` -> "AC3"). Empty for codecs we don't tag. Covers both subtitle and
/// audio codecs so every track row can show its format like PotPlayer does.
fn track_format(codec: &str) -> &'static str {
    match codec.to_ascii_lowercase().as_str() {
        // subtitles
        "subrip" | "srt" => "SRT",
        "ass" => "ASS",
        "ssa" => "SSA",
        "webvtt" => "VTT",
        "hdmv_pgs_subtitle" | "pgs" => "PGS",
        "dvd_subtitle" => "VobSub",
        "dvb_subtitle" => "DVB",
        "mov_text" => "TX3G",
        "microdvd" => "SUB",
        // audio
        "aac" => "AAC",
        "ac3" => "AC3",
        "eac3" => "E-AC3",
        "dts" => "DTS",
        "truehd" => "TrueHD",
        "flac" => "FLAC",
        "opus" => "Opus",
        "mp3" => "MP3",
        "vorbis" => "Vorbis",
        "pcm_s16le" | "pcm_s24le" | "pcm_s32le" => "PCM",
        _ => "",
    }
}

/// Friendly track label: "German", "English · SDH", "Korean (SRT)" — the full
/// language name (mapped from the code), the title when it adds information, and
/// the format tag from the codec. Falls back to the title, then "Track N".
pub fn track_label(title: &str, lang: &str, codec: &str, id: i64) -> String {
    let lang = lang_name(lang);
    let base = if !lang.is_empty() && !title.is_empty() && !title.eq_ignore_ascii_case(&lang) {
        format!("{lang} · {title}")
    } else if !lang.is_empty() {
        lang
    } else if !title.is_empty() {
        title.to_string()
    } else {
        format!("Track {id}")
    };
    let fmt = track_format(codec);
    if fmt.is_empty() { base } else { format!("{base} ({fmt})") }
}

#[cfg(test)]
mod tests {
    use super::{lang_name, track_label};

    #[test]
    fn lang_name_maps_codes_regions_and_2letter() {
        assert_eq!(lang_name("en-US"), "English");   // region stripped
        assert_eq!(lang_name("de-DE"), "German");
        assert_eq!(lang_name("he"), "Hebrew");        // 2-letter
        assert_eq!(lang_name("hu"), "Hungarian");
        assert_eq!(lang_name("lt"), "Lithuanian");
        assert_eq!(lang_name("pt-BR"), "Portuguese (BR)"); // regional variant kept
        assert_eq!(lang_name("zh-TW"), "Chinese (Traditional)");
        assert_eq!(lang_name(""), "");
        assert_eq!(lang_name("xyz"), "xyz");          // unknown -> raw
    }

    #[test]
    fn track_label_combines_language_title_and_format() {
        assert_eq!(track_label("", "en-US", "subrip", 1), "English (SRT)");
        assert_eq!(track_label("SDH", "eng", "ass", 2), "English · SDH (ASS)");
        // Title equal to the language name isn't duplicated.
        assert_eq!(track_label("Korean", "kor", "subrip", 3), "Korean (SRT)");
        // No language: fall back to the title.
        assert_eq!(track_label("Latin American", "", "subrip", 4), "Latin American (SRT)");
        // Nothing useful: numbered track.
        assert_eq!(track_label("", "", "", 5), "Track 5");
        // Audio codec tag too.
        assert_eq!(track_label("", "ja", "aac", 6), "Japanese (AAC)");
    }
}
