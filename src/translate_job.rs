//! Subtitle-translation orchestration. Ported from the WebView build's
//! `commands/translate.rs`: extract → chunk → fan-out (Semaphore-bounded) →
//! reassemble → add to mpv as an external track. The Tauri `app.emit` progress
//! is replaced by a `progress` callback the caller marshals to the UI thread.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::Semaphore;
use tracing::warn;

use crate::mpv::player::MpvPlayer;
use crate::services::{subtitle_extract, tracks::TracksService, translate};

/// Caps concurrent Google-Translate requests; more triggers 429s.
const MAX_CONCURRENT_CHUNKS: usize = 8;

/// Pause before each chunk of the retry pass. The failures being retried are
/// rate limiting, so arriving late is the whole point — hurrying would just
/// rebuild the burst that caused them.
const RETRY_PAUSE: std::time::Duration = std::time::Duration::from_millis(600);

/// Image-based subtitle codecs we cannot extract text from.
const UNSUPPORTED_CODECS: &[&str] = &["hdmv_pgs_subtitle", "dvd_subtitle", "dvb_subtitle", "pgs"];

/// Translation requests are batched into chunks of roughly this many characters
/// (the upstream service accepts ~5000/req; we stay under to leave headroom for
/// the `\n\n` separators joining entries).
const CHUNK_MAX_CHARS: usize = 4500;

/// The two halves of the job, so the panel can say which one is running.
/// Extraction has no progress to report — ffmpeg either finishes or does not —
/// and on a large file it is by far the longer half, which is exactly when
/// "0%" beside a spinner reads as a hang.
pub const PHASE_EXTRACTING: &str = "extracting";
pub const PHASE_TRANSLATING: &str = "translating";

/// What a finished run produced.
///
/// `partial` is the case worth naming: the file is written and selected like any
/// other, but some of its lines are still in the source language because the
/// service refused those chunks twice. It reads as a success everywhere except
/// in the middle of the film, so the caller is told rather than left to guess.
pub struct Outcome {
    pub partial: bool,
}

/// Abandon whatever translation is running.
///
/// Bumping the run id is all it takes: the in-flight run checks it before
/// applying anything and stands down on its own. It does keep reading until
/// ffmpeg finishes — the result still lands in the cache, so the time already
/// spent is not thrown away — but nothing reaches mpv.
pub fn cancel() {
    current_run_id().fetch_add(1, Ordering::SeqCst);
}

fn current_run_id() -> &'static AtomicU64 {
    static R: OnceLock<AtomicU64> = OnceLock::new();
    R.get_or_init(|| AtomicU64::new(0))
}

struct LastTranslation {
    path: String,
    source_sid: i64,
}

fn last_translation() -> &'static Mutex<Option<LastTranslation>> {
    static S: OnceLock<Mutex<Option<LastTranslation>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

/// Removes the previous translation track and returns the source `sid` it was
/// derived from (so callers can restore it as a fallback).
fn remove_previous_translation(mpv: &MpvPlayer) -> Option<i64> {
    let prev = match last_translation().lock() { Ok(mut g) => g.take(), Err(_) => None }?;
    let tracks = TracksService::get_all(mpv);
    if let Some(t) = tracks.iter().find(|t| t.external && t.external_filename == prev.path) {
        if let Err(e) = mpv.command(&["sub-remove", &t.id.to_string()]) {
            warn!(track_id = t.id, error = %e, "translate: sub-remove failed");
        }
    }
    let _ = std::fs::remove_file(partial_marker(&prev.path));
    if let Err(e) = std::fs::remove_file(&prev.path) {
        warn!(error = %e, "translate: temp file remove failed");
    }
    Some(prev.source_sid)
}

/// Remove the loaded translation track. Called when the user picks "Off".
pub fn clear_translation(mpv: &MpvPlayer) {
    if let Some(sid) = remove_previous_translation(mpv) {
        let _ = mpv.set::<&str>("sid", &sid.to_string());
    }
}

/// Whether selecting sub track `id` should (re)trigger a translation.
///
/// Only our own output is refused, since translating a translation is a circle.
/// An image-based track is deliberately not refused, even though no text can be
/// pulled from one: `run` answers those by falling back to a text track in the
/// same file, and reports `image-based` to the banner when the file has none.
/// Either of those is an answer — screening them out here produced neither, and
/// picking a PGS track with translation on did nothing at all, silently.
pub fn is_translatable_source(mpv: &MpvPlayer, id: i64) -> bool {
    let tracks = TracksService::get_all(mpv);
    let Some(t) = tracks.iter().find(|t| t.track_type == "sub" && t.id == id) else { return false };
    !(t.external && t.external_filename.contains("vayou-translate"))
}

/// Translate the selected subtitle into `target_lang`, adding it to mpv. Calls
/// `progress(current, total, done, phase)` as the job advances.
///
/// On failure returns a short, stable error code (`no-file`, `no-track`,
/// `image-based`, `no-entries`, `rate-limited`) that the UI maps to a localized
/// message (see `TrErrorBanner` in `ui/subtitle-panel.slint`); an empty code
/// means "fail silently, show no banner". Errors bubbled up from the extract /
/// write helpers stay as their raw text and fall through to the banner verbatim.
///
/// Succeeding is not the same as being complete: see `Outcome::partial`.
pub async fn run<F>(mpv: Arc<MpvPlayer>, target_lang: String, progress: F) -> Result<Outcome, String>
where
    F: Fn(usize, usize, bool, &'static str) + Send + Sync + 'static,
{
    let my_run = current_run_id().fetch_add(1, Ordering::SeqCst) + 1;

    let prev_translation_source = remove_previous_translation(&mpv);

    // Resolve the video from what mpv is actually playing — not a stored
    // "current file", which goes stale on playlist navigation (we'd translate
    // the previous episode's subs).
    let video_path = mpv.get_property_string("path").ok()
        .filter(|p| !p.is_empty())
        .ok_or("no-file")?;

    let tracks = TracksService::get_all(&mpv);
    let sub_track = tracks.iter()
        .find(|t| t.track_type == "sub" && t.selected)
        .or_else(|| {
            let sid = prev_translation_source?;
            mpv.set::<&str>("sid", &sid.to_string()).ok();
            tracks.iter().find(|t| t.track_type == "sub" && t.id == sid)
        })
        .ok_or("no-track")?;

    // PGS is the default subtitle on most remuxes, so the selected track is
    // routinely one there is no text to translate, and refusing outright made
    // the feature fail on exactly the files that need it. Asking to translate
    // means asking for the dialogue, so fall back to a text track — preferring
    // one in the same language, since taking merely the first picks whatever
    // the muxer put there, which on a Chinese remux is the Chinese subtitle.
    let sub_track = if UNSUPPORTED_CODECS.contains(&sub_track.codec.as_str()) {
        let is_text = |t: &&crate::services::tracks::TrackInfo| {
            t.track_type == "sub" && !UNSUPPORTED_CODECS.contains(&t.codec.as_str())
        };
        let text_track = tracks
            .iter()
            .find(|t| is_text(t) && !t.lang.is_empty() && t.lang == sub_track.lang)
            .or_else(|| tracks.iter().find(is_text));
        let Some(t) = text_track else {
            return Err("image-based".into());
        };
        mpv.set::<&str>("sid", &t.id.to_string()).ok();
        t
    } else {
        sub_track
    };

    let source_sid = sub_track.id;
    let is_ass = sub_track.codec == "ass" || sub_track.codec == "ssa"
        || (sub_track.external && matches!(Path::new(&sub_track.external_filename).extension().and_then(|e| e.to_str()), Some("ass" | "ssa")));
    let out_ext = if is_ass { "ass" } else { "srt" };
    let out_path = build_sub_path(&video_path, source_sid, &target_lang, out_ext);

    // Everything below — the ffmpeg demux and the network round-trips — is
    // deterministic for a given video, source track and language. Doing it
    // again re-reads the whole container to arrive at a byte-identical file:
    // 134s on a 22 GB remux. Removing and re-adding a translation is routine,
    // so without this the expensive path ran more than once per film.
    if Path::new(&out_path).exists() && !Path::new(&partial_marker(&out_path)).exists() {
        mpv.command(&["sub-add", &out_path, "select", lang_to_name(&target_lang), &target_lang])
            .map_err(|e| e.to_string())?;
        if let Ok(mut g) = last_translation().lock() {
            *g = Some(LastTranslation { path: out_path.clone(), source_sid });
        }
        progress(1, 1, true, PHASE_TRANSLATING);
        return Ok(Outcome { partial: false });
    }

    progress(0, 0, false, PHASE_EXTRACTING);

    // Header and entries together: for an embedded ASS track they come from a
    // single ffmpeg run, where fetching them separately demuxed the file twice.
    let external_path = (sub_track.external && !sub_track.external_filename.is_empty())
        .then_some(sub_track.external_filename.as_str());
    let (ass_header, entries) = match (external_path, is_ass) {
        (Some(file), true) => (
            subtitle_extract::extract_ass_header_from_file(file).ok(),
            subtitle_extract::extract_from_ass(file)?,
        ),
        (Some(file), false) => (None, subtitle_extract::extract_from_srt(file)?),
        (None, true) => {
            let (header, entries) =
                subtitle_extract::extract_ass_from_video(&video_path, source_sid).await?;
            (Some(header), entries)
        }
        (None, false) => (
            None,
            subtitle_extract::extract_srt_from_video(&video_path, source_sid).await?,
        ),
    };

    if entries.is_empty() {
        return Err("no-entries".into());
    }

    let chunks = chunk_entries(&entries, CHUNK_MAX_CHARS);
    let total = chunks.len();
    // Extraction is done; from here the chunks give a real percentage.
    progress(0, total, false, PHASE_TRANSLATING);
    let lang = Arc::new(target_lang);
    let entries_arc = Arc::new(entries.clone());
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT_CHUNKS));
    let progress = Arc::new(progress);

    let mut handles = Vec::with_capacity(total);
    for (idx, indices) in chunks.into_iter().enumerate() {
        let (lang, entries_ref, sem_c, prog) = (lang.clone(), entries_arc.clone(), sem.clone(), progress.clone());
        handles.push(tokio::spawn(async move {
            let _permit = sem_c.acquire_owned().await.ok();
            let combined: String = indices.iter().map(|&i| entries_ref[i].text.as_str()).collect::<Vec<_>>().join("\n\n");
            let result = translate::translate(&combined, &lang).await;
            prog(idx + 1, total, false, PHASE_TRANSLATING);
            (indices, result)
        }));
    }

    let mut translated = entries;
    let mut failed: Vec<Vec<usize>> = Vec::new();
    for h in handles {
        let (indices, result) = h.await.map_err(|e| e.to_string())?;
        match result {
            Ok(t) => apply_chunk(&mut translated, &indices, &t),
            Err(e) => {
                warn!(error = %e, "translate: chunk failed");
                failed.push(indices);
            }
        }
    }

    // Second pass, one chunk at a time.
    //
    // A chunk that gets here has already retried three times inside the request
    // helper and lost — but it lost while seven siblings were hammering the same
    // endpoint, which is what the 429s are: our own burst. Coming back after it
    // has passed, serially and unhurried, is a different request entirely, and
    // recovers what would otherwise ship as untranslated lines in the middle of
    // a finished subtitle.
    if !failed.is_empty() {
        let mut lost = Vec::new();
        for indices in failed {
            tokio::time::sleep(RETRY_PAUSE).await;
            let combined: String =
                indices.iter().map(|&i| entries_arc[i].text.as_str()).collect::<Vec<_>>().join("\n\n");
            match translate::translate(&combined, &lang).await {
                Ok(t) => apply_chunk(&mut translated, &indices, &t),
                Err(e) => {
                    warn!(error = %e, "translate: chunk failed again");
                    lost.push(indices);
                }
            }
        }
        failed = lost;
    }

    let failed_chunks = failed.len();
    if failed_chunks == total {
        return Err("rate-limited".into());
    }
    let partial = failed_chunks > 0;
    if partial {
        warn!(failed = failed_chunks, total, "translate: some chunks survived the retry — the output is partially translated");
    }

    // Write the result before deciding whether to apply it. Both checks below
    // reject work that is already paid for in full — the demux and every network
    // round-trip are done by this point — and leaving the file on disk is what
    // makes coming back to this film instant instead of another full extraction.
    if let Some(header) = ass_header {
        subtitle_extract::write_ass(&translated, &header, &out_path)?;
    } else {
        subtitle_extract::write_srt(&translated, &out_path)?;
    }
    // Flag the file as incomplete, or clear a flag an earlier attempt left, so
    // the cache above only hands back a translation that is actually whole.
    if partial {
        let _ = std::fs::write(partial_marker(&out_path), b"");
    } else {
        let _ = std::fs::remove_file(partial_marker(&out_path));
    }

    if current_run_id().load(Ordering::SeqCst) != my_run {
        // Superseded by a newer run — fail silently (empty code shows no banner).
        // The file above is the point: the run that replaced this one, or a later
        // return to this language, loads it from the cache instead of re-earning it.
        return Err(String::new());
    }

    // Bail if the film changed while we worked. Extracting an embedded track
    // from a large file takes minutes, which easily outlives the file it was
    // started for, and applying the result then drops one film's subtitles onto
    // another. The file was written above, so returning to this one is instant.
    let still_playing = mpv.get_property_string("path").unwrap_or_default();
    if still_playing != video_path {
        return Err("file-changed".into());
    }

    let lang_str = lang.as_str();
    mpv.command(&["sub-add", &out_path, "select", lang_to_name(lang_str), lang_str]).map_err(|e| e.to_string())?;
    if let Ok(mut g) = last_translation().lock() {
        *g = Some(LastTranslation { path: out_path.clone(), source_sid });
    }
    progress(total, total, true, PHASE_TRANSLATING);
    Ok(Outcome { partial })
}

/// Copy one chunk's translated text back onto the entries it was built from.
/// The service returns the chunk as one string with the entries still separated
/// by the blank line they were joined on.
fn apply_chunk(translated: &mut [subtitle_extract::SubEntry], indices: &[usize], text: &str) {
    let parts: Vec<&str> = text.split("\n\n").collect();
    for (j, &idx) in indices.iter().enumerate() {
        if j < parts.len() && idx < translated.len() {
            translated[idx].text = parts[j].trim().to_string();
        }
    }
}

/// Marker written beside a translation that came out incomplete.
///
/// The cache is keyed only on (video, source track, language), so without this a
/// run that lost chunks to rate limiting would be handed back on every later
/// open — a permanently half-translated subtitle, and no way to ask for a better
/// one short of deleting the file by hand.
fn partial_marker(out_path: &str) -> String {
    format!("{out_path}.partial")
}

/// Group entry indices into chunks of roughly `max_chars` characters (each entry
/// counts as `text.len() + 2` for the `\n\n` join). An entry longer than the
/// budget still gets its own chunk rather than being split. Pure — unit-tested.
fn chunk_entries(entries: &[subtitle_extract::SubEntry], max_chars: usize) -> Vec<Vec<usize>> {
    let mut chunks: Vec<Vec<usize>> = Vec::new();
    let (mut cur, mut len) = (Vec::new(), 0usize);
    for (i, e) in entries.iter().enumerate() {
        let l = e.text.len() + 2;
        if len + l > max_chars && !cur.is_empty() {
            chunks.push(cur);
            cur = Vec::new();
            len = 0;
        }
        cur.push(i);
        len += l;
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

fn lang_to_name(code: &str) -> &'static str {
    match code {
        "pt" => "Português", "en" => "English", "es" => "Español", "fr" => "Français",
        "de" => "Deutsch", "it" => "Italiano", "ja" => "日本語", "ko" => "한국어",
        "zh" => "中文", "ru" => "Русский", "ar" => "العربية", "hi" => "हिन्दी", _ => "Translated",
    }
}

fn build_sub_path(video_path: &str, source_sid: i64, lang: &str, ext: &str) -> String {
    let stem = Path::new(video_path).file_stem().and_then(|s| s.to_str()).unwrap_or("sub");
    let dir = std::env::temp_dir().join("vayou-translate");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("{stem}.{source_sid}.{lang}.{ext}")).to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::{chunk_entries, CHUNK_MAX_CHARS};
    use crate::services::subtitle_extract::SubEntry;

    fn entry(text: &str) -> SubEntry {
        SubEntry { start_ms: 0, end_ms: 0, text: text.into(), style: "Default".into() }
    }

    #[test]
    fn empty_input_yields_no_chunks() {
        assert!(chunk_entries(&[], CHUNK_MAX_CHARS).is_empty());
    }

    #[test]
    fn fits_in_one_chunk() {
        let entries = vec![entry("a"), entry("b"), entry("c")];
        let chunks = chunk_entries(&entries, CHUNK_MAX_CHARS);
        assert_eq!(chunks, vec![vec![0, 1, 2]]);
    }

    #[test]
    fn splits_when_budget_exceeded() {
        // Each entry counts as len+2; with text "....." (5) that's 7 chars.
        // Budget 16 fits two (14) but not three (21) → chunks of 2.
        let entries: Vec<SubEntry> = (0..5).map(|_| entry("xxxxx")).collect();
        let chunks = chunk_entries(&entries, 16);
        assert_eq!(chunks, vec![vec![0, 1], vec![2, 3], vec![4]]);
    }

    #[test]
    fn oversized_entry_gets_its_own_chunk() {
        let big = "x".repeat(CHUNK_MAX_CHARS + 100);
        let entries = vec![entry("small"), entry(&big), entry("small")];
        let chunks = chunk_entries(&entries, CHUNK_MAX_CHARS);
        // The huge middle entry is isolated; neighbours never merge across it.
        assert_eq!(chunks, vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn every_index_appears_exactly_once_in_order() {
        let entries: Vec<SubEntry> = (0..50).map(|i| entry(&format!("line {i}"))).collect();
        let flat: Vec<usize> = chunk_entries(&entries, 40).into_iter().flatten().collect();
        assert_eq!(flat, (0..50).collect::<Vec<_>>());
    }
}
