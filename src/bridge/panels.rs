//! Side-panel data: per-panel refresh dispatch, the media-info rows, and the
//! right-click context menu.

use std::sync::Arc;

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::bridge::playback::load_chapters;
use crate::bridge::playlist::refresh_playlist;
use crate::bridge::settings::push_settings;
use crate::bridge::subtitle::push_sub_style;
use crate::bridge::tracks::track_rows;
use crate::mpv::player::MpvPlayer;
use crate::services::media_info::MediaInfoService;
use crate::services::video::VideoService;
use crate::state::{AppState, MpvState};
use crate::util;
use crate::{InfoRow, MainWindow};

/// Collect the media-info rows (skipping empty values).
fn collect_info_rows(mpv: &MpvPlayer) -> Vec<InfoRow> {
    let m = MediaInfoService::get(mpv);
    let mut rows = Vec::new();
    let mut add = |label: &str, value: String| {
        if !value.is_empty() {
            rows.push(InfoRow { label: label.into(), value: value.into() });
        }
    };
    add("File", m.filename);
    if m.width > 0 { add("Resolution", format!("{}×{}", m.width, m.height)); }
    add("Video Codec", m.video_codec);
    add("Audio Codec", m.audio_codec);
    if m.fps > 0.0 { add("FPS", format!("{:.2}", m.fps)); }
    if m.video_bitrate > 0 { add("Video Bitrate", util::fmt_bitrate(m.video_bitrate)); }
    if m.audio_bitrate > 0 { add("Audio Bitrate", util::fmt_bitrate(m.audio_bitrate)); }
    add("Duration", util::fmt_duration(m.duration));
    if m.file_size > 0 { add("File Size", util::fmt_size(m.file_size)); }
    rows
}

/// Refresh the data for the panel that just opened.
pub(crate) fn refresh_panel(ui: &MainWindow, mpv_state: &Arc<MpvState>, app_state: &Arc<AppState>, panel: &str) {
    let Ok(mpv) = mpv_state.get() else { return };
    match panel {
        "sub" => {
            let rows = track_rows(mpv, "sub");
            ui.set_sub_active(rows.iter().any(|r| r.selected));
            ui.set_panel_sub_tracks(ModelRc::new(VecModel::from(rows)));
            ui.set_sub_delay(mpv.get::<f64>("sub-delay").unwrap_or(0.0) as f32);
            if let Ok(tl) = app_state.with(|s, _| s.translate_lang.clone()) {
                ui.set_translate_lang(tl.into());
            }
            push_sub_style(ui, app_state);
            let title = ui.get_media_title().to_string();
            let stem = title.rsplit_once('.').map_or(title.as_str(), |(s, _)| s);
            ui.set_search_query(stem.into());
        }
        "audio" => {
            ui.set_panel_audio_tracks(ModelRc::new(VecModel::from(track_rows(mpv, "audio"))));
            ui.set_audio_delay(mpv.get::<f64>("audio-delay").unwrap_or(0.0) as f32);
        }
        "playlist" => refresh_playlist(ui, mpv),
        "info" => ui.set_info_rows(ModelRc::new(VecModel::from(collect_info_rows(mpv)))),
        "settings" => push_settings(ui, app_state),
        _ => {}
    }
}

/// Fetch fresh track / chapter / aspect data, position the menu, and show it.
fn open_context_menu(ui: &MainWindow, mpv_state: &Arc<MpvState>, x: f32, y: f32) {
    if let Ok(mpv) = mpv_state.get() {
        ui.set_ctx_sub_tracks(ModelRc::new(VecModel::from(track_rows(mpv, "sub"))));
        ui.set_ctx_audio_tracks(ModelRc::new(VecModel::from(track_rows(mpv, "audio"))));
        load_chapters(ui, mpv);
        ui.set_current_aspect(util::match_aspect(&VideoService::get_aspect_ratio(mpv)).into());
    }
    ui.set_ctx_x(x);
    ui.set_ctx_y(y);
    ui.set_ctx_page("main".into());
    ui.set_ctx_show(true);
}

pub(crate) fn wire(ui: &MainWindow, mpv_state: &Arc<MpvState>, app_state: &Arc<AppState>) {
    ui.on_request_context_menu({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move |x, y| { if let Some(ui) = ui_w.upgrade() { open_context_menu(&ui, &mpv, x, y); } }
    });
    ui.on_open_info({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move || { if let Some(ui) = ui_w.upgrade() { ui.set_active_panel("info".into()); refresh_panel(&ui, &mpv, &app, "info"); } }
    });
    ui.on_panel_opened({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move |p| { if let Some(ui) = ui_w.upgrade() { refresh_panel(&ui, &mpv, &app, p.as_str()); } }
    });
}
