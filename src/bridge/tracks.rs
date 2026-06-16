//! Audio/subtitle track selection: the track-row builder, cycling, per-file
//! persistence, and the track/delay/external-subtitle callbacks.

use std::sync::Arc;

use slint::ComponentHandle;

use crate::bridge::panels::refresh_panel;
use crate::bridge::subtitle::start_translation;
use crate::error::LogErr;
use crate::mpv::player::MpvPlayer;
use crate::services::tracks::TracksService;
use crate::state::{AppState, MpvState};
use crate::util;
use crate::{MainWindow, TrackRow};

/// Build the track rows for the context menu / panels (one `kind`: sub/audio).
pub(crate) fn track_rows(mpv: &MpvPlayer, kind: &str) -> Vec<TrackRow> {
    TracksService::get_all(mpv)
        .into_iter()
        .filter(|t| t.track_type == kind)
        .map(|t| TrackRow {
            id: t.id as i32,
            label: util::track_label(&t.title, &t.lang, &t.codec, t.id).into(),
            selected: t.selected,
        })
        .collect()
}

/// Select the next sub/audio track in cyclic order (keyboard shortcut).
pub(crate) fn cycle_track(mpv_state: &Arc<MpvState>, kind: &str) {
    let Ok(mpv) = mpv_state.get() else { return };
    let tracks: Vec<_> = TracksService::get_all(mpv).into_iter().filter(|t| t.track_type == kind).collect();
    if tracks.is_empty() {
        return;
    }
    let cur = tracks.iter().position(|t| t.selected);
    let next = cur.map_or(0, |i| (i + 1) % tracks.len());
    let id = tracks[next].id;
    if kind == "sub" {
        TracksService::select_subtitle(mpv, id).log_err("cycle subtitle track");
    } else {
        TracksService::select_audio(mpv, id).log_err("cycle audio track");
    }
}

/// Select a subtitle / audio track and persist the choice (per-file) like the
/// WebView build did.
fn select_track(mpv_state: &Arc<MpvState>, app_state: &Arc<AppState>, kind: &str, id: i64) {
    let Ok(mpv) = mpv_state.get() else { return };
    if kind == "sub" {
        TracksService::select_subtitle(mpv, id).log_err("select subtitle track");
    } else {
        TracksService::select_audio(mpv, id).log_err("select audio track");
    }
    let _ = app_state.with(|settings, current_file| {
        if !settings.remember_selections { return; }
        let Some(path) = current_file.as_deref() else { return };
        let saved = if id < 0 { None } else { Some(id) };
        if kind == "sub" { settings.set_sub_track(path, saved); } else { settings.set_audio_track(path, saved); }
        settings.save().log_err("save track selection");
    });
}

pub(crate) fn wire(ui: &MainWindow, mpv_state: &Arc<MpvState>, app_state: &Arc<AppState>) {
    ui.on_select_sub({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move |id| {
            let id = i64::from(id);
            select_track(&mpv, &app, "sub", id);
            let Some(ui) = ui_w.upgrade() else { return };
            refresh_panel(&ui, &mpv, &app, ui.get_active_panel().as_str());
            // With translation active, re-translate the newly picked track —
            // skip "Disable", image-based subs, and our own translation output.
            if id >= 0 && ui.get_translate_lang().as_str() != "off" {
                if let Ok(m) = mpv.get() {
                    if crate::translate_job::is_translatable_source(m, id) {
                        start_translation(&ui, &mpv, &app);
                    }
                }
            }
        }
    });
    ui.on_select_audio({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move |id| {
            select_track(&mpv, &app, "audio", i64::from(id));
            if let Some(ui) = ui_w.upgrade() { refresh_panel(&ui, &mpv, &app, ui.get_active_panel().as_str()); }
        }
    });
    ui.on_set_sub_delay({
        let mpv = mpv_state.clone();
        move |v| { if let Ok(m) = mpv.get() { TracksService::set_subtitle_delay(m, f64::from(v)).log_err("set subtitle delay"); } }
    });
    ui.on_set_audio_delay({
        let mpv = mpv_state.clone();
        move |v| { if let Ok(m) = mpv.get() { TracksService::set_audio_delay(m, f64::from(v)).log_err("set audio delay"); } }
    });
    ui.on_load_external_sub({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move || {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("Subtitles", &["srt", "ass", "ssa", "sub", "vtt", "idx", "sup"])
                .add_filter("All", &["*"]).pick_file() else { return };
            let Some(ui) = ui_w.upgrade() else { return };
            if let Ok(m) = mpv.get() {
                match TracksService::load_subtitle(m, &path.to_string_lossy()) {
                    Ok(()) => refresh_panel(&ui, &mpv, &app, "sub"),
                    Err(e) => {
                        tracing::warn!(error = %e, "load external subtitle");
                        ui.set_toast("Couldn't load that subtitle file".into());
                    }
                }
            }
        }
    });
}
