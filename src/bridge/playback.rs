//! Core playback: play/pause, seek, volume, speed, mute, A-B loop, screenshot,
//! chapters, and the sleep timer — plus the helpers the keyboard handler reuses.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::error::LogErr;
use crate::mpv::player::MpvPlayer;
use crate::services::playback::PlaybackService;
use crate::state::{self, AppState, MpvState};
use crate::{ChapterRow, MainWindow};

/// Volume ceiling: 200 % when boost is on, else 100 %.
pub(crate) fn max_volume(app_state: &Arc<AppState>) -> f32 {
    if app_state.with(|s, _| s.volume_boost).unwrap_or(false) { 200.0 } else { 100.0 }
}

fn set_volume(ui: &MainWindow, mpv_state: &Arc<MpvState>, vol: f32) {
    ui.set_volume(vol);
    ui.set_muted(vol == 0.0);
    if let Ok(mpv) = mpv_state.get() {
        let _ = PlaybackService::set_volume(mpv, f64::from(vol));
    }
}

pub(crate) fn adjust_volume(ui: &MainWindow, mpv_state: &Arc<MpvState>, app_state: &Arc<AppState>, delta: f32) {
    let vol = (ui.get_volume() + delta).clamp(0.0, max_volume(app_state));
    set_volume(ui, mpv_state, vol);
}

pub(crate) fn toggle_mute(ui: &MainWindow, mpv_state: &Arc<MpvState>) {
    let muted = !ui.get_muted();
    ui.set_muted(muted);
    // Unmuting while the level sits at 0 would stay silent — restore an audible
    // default so the toggle always has an effect.
    if !muted && ui.get_volume() <= 0.0 {
        ui.set_volume(100.0);
    }
    if let Ok(mpv) = mpv_state.get() {
        let _ = PlaybackService::set_volume(mpv, if muted { 0.0 } else { f64::from(ui.get_volume()) });
    }
}

pub(crate) fn set_speed(ui: &MainWindow, mpv_state: &Arc<MpvState>, speed: f32) {
    let s = (speed * 100.0).round() / 100.0;
    ui.set_speed(s);
    if let Ok(mpv) = mpv_state.get() {
        PlaybackService::set_speed(mpv, f64::from(s)).log_err("set speed");
    }
}

fn take_screenshot(mpv_state: &Arc<MpvState>) -> Result<String, String> {
    let mpv = mpv_state.get().map_err(|e| e.to_string())?;
    let dir = dirs::picture_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default()).join("Vayou");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create folder: {e}"))?;
    let name = chrono::Local::now().format("vayou_%Y%m%d_%H%M%S.png").to_string();
    let path = dir.join(name);
    PlaybackService::screenshot(mpv, &path.to_string_lossy()).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// Take a screenshot and surface the outcome in the UI toast — so a failed
/// grab (read-only Pictures folder, no video) isn't silently a no-op.
pub(crate) fn screenshot_to_toast(ui: &MainWindow, mpv_state: &Arc<MpvState>) {
    match take_screenshot(mpv_state) {
        Ok(_) => ui.set_toast("Screenshot saved".into()),
        Err(e) => {
            tracing::warn!(error = %e, "screenshot");
            ui.set_toast(format!("Screenshot failed: {e}").into());
        }
    }
}

/// Read chapters from mpv and push them to the seek bar.
pub(crate) fn load_chapters(ui: &MainWindow, mpv: &MpvPlayer) {
    let rows: Vec<ChapterRow> = PlaybackService::get_chapters(mpv)
        .into_iter()
        .map(|c| ChapterRow { time: c.time as f32, title: c.title.into(), current: c.current })
        .collect();
    ui.set_chapters(ModelRc::new(VecModel::from(rows)));
}

/// Push the AB-loop endpoints (and enabled flag) to the UI.
pub(crate) fn push_ab(ui: &MainWindow) {
    let a = state::ab_loop::get_a();
    let b = state::ab_loop::get_b();
    ui.set_ab_a(a.map_or(-1.0, |v| v as f32));
    ui.set_ab_b(b.map_or(-1.0, |v| v as f32));
    ui.set_ab_enabled(a.is_some() || b.is_some());
}

fn fmt_clock(secs: i32) -> String {
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

pub(crate) fn wire(ui: &MainWindow, mpv_state: &Arc<MpvState>, app_state: &Arc<AppState>) {
    ui.on_toggle_pause({
        let mpv = mpv_state.clone();
        move || { if let Ok(m) = mpv.get() { PlaybackService::toggle_pause(m).log_err("toggle pause"); } }
    });
    ui.on_prev_file({
        let mpv = mpv_state.clone();
        move || { if let Ok(m) = mpv.get() { crate::services::playlist::PlaylistService::prev(m).log_err("previous file"); } }
    });
    ui.on_next_file({
        let mpv = mpv_state.clone();
        move || { if let Ok(m) = mpv.get() { crate::services::playlist::PlaylistService::next(m).log_err("next file"); } }
    });
    ui.on_seek_fraction({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move |frac| {
            let Some(ui) = ui_w.upgrade() else { return };
            let t = f64::from(frac) * f64::from(ui.get_duration());
            if let Ok(m) = mpv.get() { let _ = PlaybackService::seek_absolute(m, t); }
        }
    });
    ui.on_adjust_volume({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move |delta| { if let Some(ui) = ui_w.upgrade() { adjust_volume(&ui, &mpv, &app, delta); } }
    });
    ui.on_set_volume({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move |v| { if let Some(ui) = ui_w.upgrade() { set_volume(&ui, &mpv, v); } }
    });
    ui.on_toggle_mute({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move || { if let Some(ui) = ui_w.upgrade() { toggle_mute(&ui, &mpv); } }
    });
    ui.on_pick_speed({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move |s| { if let Some(ui) = ui_w.upgrade() { set_speed(&ui, &mpv, s); } }
    });
    ui.on_ab_set_a({
        let ui_w = ui.as_weak();
        move || {
            let Some(ui) = ui_w.upgrade() else { return };
            PlaybackService::set_ab_loop_a(Some(f64::from(ui.get_current_time())));
            push_ab(&ui);
        }
    });
    ui.on_ab_set_b({
        let ui_w = ui.as_weak();
        move || {
            let Some(ui) = ui_w.upgrade() else { return };
            PlaybackService::set_ab_loop_b(Some(f64::from(ui.get_current_time())));
            push_ab(&ui);
        }
    });
    ui.on_ab_clear({
        let ui_w = ui.as_weak();
        move || {
            let Some(ui) = ui_w.upgrade() else { return };
            PlaybackService::clear_ab_loop();
            push_ab(&ui);
        }
    });
    ui.on_ab_toggle({
        let ui_w = ui.as_weak();
        move || {
            let Some(ui) = ui_w.upgrade() else { return };
            if ui.get_ab_enabled() {
                PlaybackService::clear_ab_loop();
                ui.set_ab_enabled(false);
                push_ab(&ui);
            } else {
                ui.set_ab_enabled(true);
            }
        }
    });
    ui.on_screenshot({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move || { if let Some(ui) = ui_w.upgrade() { screenshot_to_toast(&ui, &mpv); } }
    });
    ui.on_seek_chapter({
        let mpv = mpv_state.clone();
        move |idx| { if let Ok(m) = mpv.get() { PlaybackService::seek_chapter(m, i64::from(idx)).log_err("seek chapter"); } }
    });

    // Sleep timer: pause playback after N minutes, ticking the remaining time.
    let sleep_timer = Rc::new(slint::Timer::default());
    let sleep_left = Rc::new(Cell::new(0i32));
    ui.on_set_sleep({
        let (ui_w, mpv, timer, left) = (ui.as_weak(), mpv_state.clone(), sleep_timer.clone(), sleep_left.clone());
        move |min| {
            left.set(min * 60);
            if let Some(ui) = ui_w.upgrade() {
                ui.set_sleep_min(min);
                ui.set_sleep_remaining(fmt_clock(min * 60).into());
            }
            let (ui_w, mpv, timer2, left2) = (ui_w.clone(), mpv.clone(), timer.clone(), left.clone());
            timer.start(slint::TimerMode::Repeated, Duration::from_secs(1), move || {
                let r = left2.get() - 1;
                if r <= 0 {
                    timer2.stop();
                    left2.set(0);
                    if let Ok(m) = mpv.get() { PlaybackService::pause(m).log_err("sleep-timer pause"); }
                    if let Some(ui) = ui_w.upgrade() { ui.set_sleep_min(0); ui.set_sleep_remaining(String::new().into()); }
                } else {
                    left2.set(r);
                    if let Some(ui) = ui_w.upgrade() { ui.set_sleep_remaining(fmt_clock(r).into()); }
                }
            });
        }
    });
    ui.on_cancel_sleep({
        let (ui_w, timer, left) = (ui.as_weak(), sleep_timer, sleep_left);
        move || {
            timer.stop();
            left.set(0);
            if let Some(ui) = ui_w.upgrade() { ui.set_sleep_min(0); ui.set_sleep_remaining(String::new().into()); }
        }
    });
}
