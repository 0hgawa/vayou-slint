//! Video adjustments (brightness/contrast/saturation, zoom/pan, aspect,
//! deinterlace) and the audio equalizer / normalization / volume-boost.

use std::sync::Arc;

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::error::LogErr;
use crate::services::audio_fx::AudioFxService;
use crate::services::playback::PlaybackService;
use crate::services::video::VideoService;
use crate::state::{AppState, MpvState};
use crate::util;
use crate::MainWindow;

const ASPECT_RATIOS: &[&str] = &["-1", "16:9", "4:3", "21:9", "2.35:1"];

/// Cycle to the next aspect-ratio preset (keyboard shortcut).
pub(crate) fn cycle_aspect(mpv_state: &Arc<MpvState>) {
    let Ok(mpv) = mpv_state.get() else { return };
    let cur = VideoService::get_aspect_ratio(mpv);
    let idx = ASPECT_RATIOS.iter().position(|r| *r == cur).unwrap_or(0);
    let next = ASPECT_RATIOS[(idx + 1) % ASPECT_RATIOS.len()];
    VideoService::set_aspect_ratio(mpv, next).log_err("cycle aspect ratio");
}

pub(crate) fn nudge_zoom(mpv_state: &Arc<MpvState>, delta: f64) {
    let Ok(mpv) = mpv_state.get() else { return };
    let z = VideoService::get_zoom_pan(mpv).zoom;
    let _ = VideoService::set_zoom(mpv, z + delta);
}

pub(crate) fn nudge_pan(mpv_state: &Arc<MpvState>, dx: f64, dy: f64) {
    let Ok(mpv) = mpv_state.get() else { return };
    let s = VideoService::get_zoom_pan(mpv);
    let _ = VideoService::set_pan(mpv, s.pan_x + dx, s.pan_y + dy);
}

/// Apply the panel's 5-band equalizer values to mpv (when enabled).
fn apply_eq(ui: &MainWindow, mpv_state: &Arc<MpvState>) {
    let bands = ui.get_eq_bands();
    let arr: [f64; 5] = std::array::from_fn(|i| f64::from(bands.row_data(i).unwrap_or(0)));
    if let Ok(m) = mpv_state.get() {
        AudioFxService::set_equalizer(m, &arr).log_err("apply equalizer");
    }
}

pub(crate) fn wire(ui: &MainWindow, mpv_state: &Arc<MpvState>, app_state: &Arc<AppState>) {
    ui.on_set_aspect({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move |ratio| {
            if let Ok(m) = mpv.get() { VideoService::set_aspect_ratio(m, ratio.as_str()).log_err("set aspect ratio"); }
            if let Some(ui) = ui_w.upgrade() { ui.set_current_aspect(util::match_aspect(ratio.as_str()).into()); }
        }
    });
    ui.on_set_brightness({ let mpv = mpv_state.clone(); move |v| { if let Ok(m) = mpv.get() { let _ = VideoService::set_brightness(m, i64::from(v)); } } });
    ui.on_set_contrast({ let mpv = mpv_state.clone(); move |v| { if let Ok(m) = mpv.get() { let _ = VideoService::set_contrast(m, i64::from(v)); } } });
    ui.on_set_saturation({ let mpv = mpv_state.clone(); move |v| { if let Ok(m) = mpv.get() { let _ = VideoService::set_saturation(m, i64::from(v)); } } });
    ui.on_set_vid_zoom({ let mpv = mpv_state.clone(); move |v| { if let Ok(m) = mpv.get() { let _ = VideoService::set_zoom(m, f64::from(v)); } } });
    ui.on_toggle_deinterlace({ let mpv = mpv_state.clone(); move || { if let Ok(m) = mpv.get() { VideoService::toggle_deinterlace(m).log_err("toggle deinterlace"); } } });
    ui.on_reset_video({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move || {
            if let Some(ui) = ui_w.upgrade() { ui.set_brightness(0); ui.set_contrast(0); ui.set_saturation(0); ui.set_vid_zoom(0.0); }
            if let Ok(m) = mpv.get() {
                let _ = VideoService::set_brightness(m, 0);
                let _ = VideoService::set_contrast(m, 0);
                let _ = VideoService::set_saturation(m, 0);
                let _ = VideoService::reset_zoom_pan(m);
            }
        }
    });
    ui.on_toggle_volume_boost({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move || {
            let Some(ui) = ui_w.upgrade() else { return };
            let boost = ui.get_volume_boost();
            let _ = app.with(|s, _| { s.volume_boost = boost; s.save().log_err("save volume boost"); });
            ui.set_max_volume(if boost { 200.0 } else { 100.0 });
            if let Ok(m) = mpv.get() {
                m.set::<&str>("volume-max", if boost { "200" } else { "100" }).log_err("set volume-max");
                if boost && ui.get_volume() <= 100.0 { ui.set_volume(130.0); PlaybackService::set_volume(m, 130.0).log_err("set volume"); }
                else if !boost && ui.get_volume() > 100.0 { ui.set_volume(100.0); PlaybackService::set_volume(m, 100.0).log_err("set volume"); }
            }
        }
    });
    ui.on_toggle_normalization({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move || { if let (Some(ui), Ok(m)) = (ui_w.upgrade(), mpv.get()) { AudioFxService::set_normalization(m, ui.get_normalization()).log_err("set normalization"); } }
    });
    ui.on_toggle_eq({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move || {
            let Some(ui) = ui_w.upgrade() else { return };
            let on = !ui.get_eq_enabled();
            ui.set_eq_enabled(on);
            let _ = app.with(|s, _| { s.equalizer_enabled = on; s.save().log_err("save equalizer toggle"); });
            if on { apply_eq(&ui, &mpv); }
            else if let Ok(m) = mpv.get() { AudioFxService::reset_equalizer(m).log_err("reset equalizer"); }
        }
    });
    ui.on_set_eq({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move |_i, _v| { if let Some(ui) = ui_w.upgrade() { if ui.get_eq_enabled() { apply_eq(&ui, &mpv); } } }
    });
    ui.on_set_eq_preset({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move |name| {
            let p: [i32; 5] = match name.as_str() {
                "Bass" => [8, 5, 0, 0, 0], "Treble" => [0, 0, 0, 4, 8], "Vocal" => [-2, 0, 4, 4, 0], "Rock" => [4, 2, -1, 2, 4], _ => [0, 0, 0, 0, 0],
            };
            if let Some(ui) = ui_w.upgrade() {
                ui.set_eq_bands(ModelRc::new(VecModel::from(p.to_vec())));
                if ui.get_eq_enabled() { apply_eq(&ui, &mpv); }
            }
        }
    });
    ui.on_reset_eq({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move || {
            if let Some(ui) = ui_w.upgrade() { ui.set_eq_bands(ModelRc::new(VecModel::from(vec![0, 0, 0, 0, 0]))); }
            if let Ok(m) = mpv.get() { AudioFxService::reset_equalizer(m).log_err("reset equalizer"); }
        }
    });
}
