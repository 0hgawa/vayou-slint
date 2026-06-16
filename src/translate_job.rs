//! Subtitle-translation orchestration. Ported from the WebView build's
//! `commands/translate.rs`: extract → chunk → fan-out (Semaphore-bounded) →
//! reassemble → add to mpv as an external track. The Tauri `app.emit` progress
//! is replaced by a `progress` callback the caller marshals to the UI thread.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::mpv::player::MpvPlayer;
use crate::services::{subtitle_extract, tracks::TracksService, translate};
use crate::state::AppState;

/// Caps concurrent Google-Translate requests; more triggers 429s.
const MAX_CONCURRENT_CHUNKS: usize = 8;

/// Image-based subtitle codecs we cannot extract text from.
const UNSUPPORTED_CODECS: &[&str] = &["hdmv_pgs_subtitle", "dvd_subtitle", "dvb_subtitle", "pgs"];

/// Translation requests are batched into chunks of roughly this many characters
/// (the upstream service accepts ~5000/req; we stay under to leave headroom for
/// the `\n\n` separators joining entries).
const CHUNK_MAX_CHARS: usize = 4500;

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
    info!(path = %prev.path, "translate: removing previous translation");
    let tracks = TracksService::get_all(mpv);
    if let Some(t) = tracks.iter().find(|t| t.external && t.external_filename == prev.path) {
        match mpv.command(&["sub-remove", &t.id.to_string()]) {
            Ok(()) => info!(track_id = t.id, "translate: sub-remove ok"),
            Err(e) => warn!(track_id = t.id, error = %e, "translate: sub-remove failed"),
        }
    }
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

/// Whether selecting sub track `id` should (re)trigger a translation: it must be
/// a real, text-based source — not an image-based codec (PGS/DVD/DVB, which we
/// can't extract text from) and not the translated track we produced ourselves.
pub fn is_translatable_source(mpv: &MpvPlayer, id: i64) -> bool {
    let tracks = TracksService::get_all(mpv);
    let Some(t) = tracks.iter().find(|t| t.track_type == "sub" && t.id == id) else { return false };
    !(UNSUPPORTED_CODECS.contains(&t.codec.as_str())
        || t.external && t.external_filename.contains("vayou-translate"))
}

/// Translate the selected subtitle into `target_lang`, adding it to mpv. Calls
/// `progress(current, total, done)` as chunks complete.
pub async fn run<F>(mpv: Arc<MpvPlayer>, app: Arc<AppState>, target_lang: String, progress: F) -> Result<String, String>
where
    F: Fn(usize, usize, bool) + Send + Sync + 'static,
{
    let my_run = current_run_id().fetch_add(1, Ordering::SeqCst) + 1;
    info!(target_lang = %target_lang, run = my_run, "translate: START");

    let prev_translation_source = remove_previous_translation(&mpv);

    let video_path = app.with(|_, f| f.clone()).map_err(|e| e.to_string())?
        .ok_or("No file playing")?;

    let tracks = TracksService::get_all(&mpv);
    let sub_track = tracks.iter()
        .find(|t| t.track_type == "sub" && t.selected)
        .or_else(|| {
            let sid = prev_translation_source?;
            mpv.set::<&str>("sid", &sid.to_string()).ok();
            tracks.iter().find(|t| t.track_type == "sub" && t.id == sid)
        })
        .ok_or("No subtitle track selected")?;

    if UNSUPPORTED_CODECS.contains(&sub_track.codec.as_str()) {
        return Err(format!("'{}' subtitles are image-based and cannot be translated", sub_track.codec));
    }

    let source_sid = sub_track.id;
    let is_ass = sub_track.codec == "ass" || sub_track.codec == "ssa"
        || (sub_track.external && matches!(Path::new(&sub_track.external_filename).extension().and_then(|e| e.to_str()), Some("ass" | "ssa")));
    let out_ext = if is_ass { "ass" } else { "srt" };
    let out_path = build_sub_path(&video_path, &target_lang, out_ext);

    progress(0, 0, false);

    let ass_header = if is_ass {
        if sub_track.external && !sub_track.external_filename.is_empty() {
            subtitle_extract::extract_ass_header_from_file(&sub_track.external_filename).ok()
        } else {
            subtitle_extract::extract_ass_header_from_video(&video_path, sub_track.id).await.ok()
        }
    } else { None };

    let entries = if sub_track.external && !sub_track.external_filename.is_empty() {
        let ext = Path::new(&sub_track.external_filename).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        match ext.as_str() {
            "ass" | "ssa" => subtitle_extract::extract_from_ass(&sub_track.external_filename),
            _ => subtitle_extract::extract_from_srt(&sub_track.external_filename),
        }
    } else {
        subtitle_extract::extract_from_video(&video_path, Some(sub_track.id), is_ass).await
    }?;

    if entries.is_empty() {
        return Err("No subtitle entries found".into());
    }
    info!(entry_count = entries.len(), "translate: entries extracted");

    let chunks = chunk_entries(&entries, CHUNK_MAX_CHARS);
    let total = chunks.len();
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
            prog(idx + 1, total, false);
            (indices, result)
        }));
    }

    let mut translated = entries;
    let mut failed_chunks = 0usize;
    for h in handles {
        let (indices, result) = h.await.map_err(|e| e.to_string())?;
        match result {
            Ok(t) => {
                let parts: Vec<&str> = t.split("\n\n").collect();
                for (j, &idx) in indices.iter().enumerate() {
                    if j < parts.len() && idx < translated.len() {
                        translated[idx].text = parts[j].trim().to_string();
                    }
                }
            }
            Err(e) => {
                failed_chunks += 1;
                warn!(error = %e, "translate: chunk failed");
            }
        }
    }
    if failed_chunks == total {
        return Err("Translation failed: all chunks were rate-limited or rejected by the upstream service".into());
    }

    if current_run_id().load(Ordering::SeqCst) != my_run {
        return Err("Superseded by a newer translation".into());
    }

    if let Some(header) = ass_header {
        subtitle_extract::write_ass(&translated, &header, &out_path)?;
    } else {
        subtitle_extract::write_srt(&translated, &out_path)?;
    }

    let lang_str = lang.as_str();
    mpv.command(&["sub-add", &out_path, "select", lang_to_name(lang_str), lang_str]).map_err(|e| e.to_string())?;
    if let Ok(mut g) = last_translation().lock() {
        *g = Some(LastTranslation { path: out_path.clone(), source_sid });
    }
    progress(total, total, true);
    info!("translate: DONE");
    Ok(out_path)
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

fn build_sub_path(video_path: &str, lang: &str, ext: &str) -> String {
    let stem = Path::new(video_path).file_stem().and_then(|s| s.to_str()).unwrap_or("sub");
    let dir = std::env::temp_dir().join("vayou-translate");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("{stem}.{lang}.{ext}")).to_string_lossy().into_owned()
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
